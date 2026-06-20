//! Calculations that are run at each cycle during operation.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write;
use std::iter::Iterator;
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::ControllerCtx;
use crate::{calc::*, peripheral::Peripheral};
use deimos_shared::states::OperatingMetrics;

/// Internal state of calc orchestrator
#[derive(Default)]
struct CalcOrchestratorState {
    /// Values of every input and output field for calcs and peripherals.
    /// Notably does not include peripheral metrics.
    calc_tape: Vec<f64>,

    /// Calc names in the order that they are evaluated at each cycle
    eval_order: Vec<CalcName>,

    /// Names of states to dispatch
    dispatch_names: Vec<FieldName>,

    /// Calc tape indices of states to dispatch
    dispatch_indices: Vec<usize>,

    /// Contiguous slices of data where each peripheral's outputs
    /// (values read from ADCs, etc) are stored
    peripheral_output_slices: BTreeMap<PeripheralName, Range<usize>>,

    /// Contiguous slices of data where each peripheral's inputs
    /// (pin states, etc) are stored
    peripheral_input_slices: BTreeMap<PeripheralName, Range<usize>>,

    /// Where to get values to write to peripheral inputs,
    /// and where to put them.
    peripheral_input_source_indices: Vec<(DstIndex, SrcIndex)>,

    /// Peripheral input fields that can be written manually, and their indices.
    manual_input_indices: BTreeMap<FieldName, usize>,
}

#[derive(Clone)]
struct DotPortEndpoint {
    node_id: String,
    port_id: String,
}

fn dot_quote_id(id: &str) -> String {
    let escaped = id.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn dot_escape_string(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn dot_escape_record_text(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '{' | '}' | '|' | '<' | '>' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn dot_record_label(center: &str, inputs: &[String], outputs: &[String]) -> String {
    let mut label = String::new();
    label.push_str("{{");

    for (i, input_name) in inputs.iter().enumerate() {
        if i > 0 {
            label.push('|');
        }
        let _ = write!(
            label,
            "<in_{i}> {}",
            dot_escape_record_text(input_name.as_str())
        );
    }

    label.push_str("}|");
    label.push_str(&dot_escape_record_text(center));
    label.push_str("|{");

    for (i, output_name) in outputs.iter().enumerate() {
        if i > 0 {
            label.push('|');
        }
        let _ = write!(
            label,
            "<out_{i}> {}",
            dot_escape_record_text(output_name.as_str())
        );
    }

    label.push_str("}}");
    label
}

/// Calculations that are run at each cycle during operation.
/// Because the results of the calculations are needed at each cycle,
/// they are run synchronously, and must complete before the next cycle.
///
/// `Calc` objects are registered with the `CalcOrchestrator` and serialized with the controller.
/// Each calc is a function consuming any number of inputs and producing any number of outputs.
///
/// During init, the orchestrator determines the correct order in which to evaluate the calcs
/// so that their inputs are updated properly, or emits and error if no such ordering is possible.
///
/// The method used for evaluating these calcs borrows from algorithmic differentiation
/// by flattening the calc expression graph into a serial "tape" of values with an established
/// evaluation order. During init, each calc is provided with the indices of the tape
/// where its inputs and outputs will be placed. During evaluation, each calc is responsible
/// for using those indices to read its inputs and write its outputs.
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct CalcOrchestrator {
    calcs: BTreeMap<CalcName, Box<dyn Calc>>,
    peripheral_input_sources: BTreeMap<PeripheralInputName, FieldName>,

    #[serde(skip)]
    state: CalcOrchestratorState,
}

impl CalcOrchestrator {
    /// Synchronize calc state using latest outputs from peripherals
    /// by running each calc in the order determined during init.
    ///
    /// # Panics
    /// * If eval() is called before init()
    /// * If evaluation of individual calcs panics
    #[inline]
    pub fn eval(&mut self) -> Result<(), String> {
        // Evaluate calcs in order
        for name in self.state.eval_order.iter() {
            self.calcs
                .get_mut(name)
                .unwrap()
                .eval(&mut self.state.calc_tape)?;
        }

        // Populate peripheral inputs
        for (dst_index, src_index) in self.state.peripheral_input_source_indices.iter().copied() {
            self.state.calc_tape[dst_index] = self.state.calc_tape[src_index];
        }
        Ok(())
    }

    /// Consume the latest outputs from a single peripheral by name,
    /// usually immediately after a packet is received.
    ///
    /// The closure interface allows parsing the outputs
    /// directly into the target range without copying.
    pub fn consume_peripheral_outputs(
        &mut self,
        name: &str,
        f: &mut dyn FnMut(&mut [f64]) -> OperatingMetrics,
    ) -> OperatingMetrics {
        let range = self.state.peripheral_output_slices[name].clone();
        f(&mut self.state.calc_tape[range])
    }

    /// Provide latest inputs (outputs of calcs) for a given peripheral
    pub fn provide_peripheral_inputs(
        &self,
        name: &str,
        mut f: impl FnMut(&mut dyn Iterator<Item = f64>),
    ) {
        let range = self.state.peripheral_input_slices[name].clone();
        let mut vals = range.map(|i| self.state.calc_tape[i]);

        f(&mut vals);
    }

    /// Provide latest values fields marked for dispatch
    #[inline]
    pub fn provide_dispatcher_outputs(&self, mut f: impl FnMut(&mut dyn Iterator<Item = f64>)) {
        let inds = self.state.dispatch_indices.iter();
        let mut vals = inds.map(|&i| self.state.calc_tape[i]);

        f(&mut vals);
    }

    /// Names of peripheral inputs that can be written manually for given peripherals.
    pub fn manual_input_names(
        &self,
        peripherals: &BTreeMap<String, Box<dyn Peripheral>>,
    ) -> Vec<String> {
        let mut names = Vec::new();
        for (name, peripheral) in peripherals.iter() {
            for field in peripheral.input_names() {
                let full = format!("{name}.{field}");
                if self.peripheral_input_sources.contains_key(&full) {
                    continue;
                }
                names.push(full);
            }
        }
        names
    }

    /// Set a manual input value by field name.
    pub fn set_manual_input(&mut self, name: &str, value: f64) -> Result<(), String> {
        match self.state.manual_input_indices.get(name) {
            Some(&index) => {
                self.state.calc_tape[index] = value;
                Ok(())
            }
            None => Err(format!(
                "Manual input `{name}` is not available for manual writes"
            )),
        }
    }

    /// Get names of fields marked to dispatch
    pub fn get_dispatch_names(&self) -> Vec<String> {
        self.state.dispatch_names.clone()
    }

    /// Get units of fields marked to dispatch, in the same order as `get_dispatch_names`.
    ///
    /// Returns `None` for peripheral inputs and outputs (which carry no declared unit) and for
    /// calc outputs whose calc does not override `get_output_units`.
    pub fn get_dispatch_units(&self) -> Vec<Option<String>> {
        self.state
            .dispatch_names
            .iter()
            .map(|field_name| {
                // Field names are "node_name.output_name". Split on the first `.`.
                let (node_name, output_name) = match field_name.split_once('.') {
                    Some(parts) => parts,
                    None => return None,
                };
                let calc = self.calcs.get(node_name)?;
                let output_names = calc.get_output_names();
                let output_index = output_names.iter().position(|n| n == output_name)?;
                calc.get_output_units().get(output_index)?.clone()
            })
            .collect()
    }

    /// Add a calc
    ///
    /// # Panics
    /// * If a calc with this name already exists
    pub fn add_calc(&mut self, name: &str, calc: Box<dyn Calc>) {
        let name = name.to_owned();
        if self.calcs.contains_key(&name) {
            panic!("A calc named `{name}` already exists.");
        }
        self.calcs.insert(name, calc);
    }

    /// Add multiple calcs.
    ///
    /// # Errors
    /// * If, for any calc to add, a calc with this name already exists
    pub fn add_calcs(&mut self, mut calcs: BTreeMap<String, Box<dyn Calc>>) -> Result<(), String> {
        for name in calcs.keys() {
            if self.calcs.contains_key(name) {
                return Err(format!("A calc named `{name}` already exists."));
            }
        }

        while let Some((name, calc)) = calcs.pop_first() {
            self.calcs.insert(name, calc);
        }

        Ok(())
    }

    /// Remove all calcs and peripheral input sources
    pub fn clear_calcs(&mut self) {
        self.calcs.clear();
        self.peripheral_input_sources.clear();
    }

    /// Read-only access to calc nodes
    pub fn calcs(&self) -> &BTreeMap<String, Box<dyn Calc>> {
        &self.calcs
    }

    /// Read-only access to edges from calcs to peripherals
    pub fn peripheral_input_sources(&self) -> &BTreeMap<PeripheralInputName, FieldName> {
        &self.peripheral_input_sources
    }

    /// Set a peripheral input to draw from a source field
    pub fn set_peripheral_input_source(&mut self, input_field: &str, source_field: &str) {
        self.peripheral_input_sources
            .insert(input_field.to_owned(), source_field.to_owned());
    }

    /// Render the current calc expression graph as Graphviz DOT with record ports.
    pub fn graphviz_dot(&self, peripherals: &BTreeMap<String, Box<dyn Peripheral>>) -> String {
        fn unknown_field_node_id(field_name: &str) -> String {
            format!("field::{field_name}")
        }

        // Formatting configuration
        let mut dot = String::new();
        dot.push_str("digraph calc_expression {\n");
        dot.push_str("  graph [nodesep=0.2, ranksep=\"3.0 equally\", concentrate=true];\n");
        dot.push_str("  rankdir=LR;\n");
        dot.push_str("  splines=true;\n");
        dot.push_str("  node [\n");
        dot.push_str("    shape=record,\n");
        dot.push_str("    fontname=\"monospace\",\n");
        dot.push_str("    margin=\"0.5,0.10\",\n");
        dot.push_str("  ];\n");
        dot.push_str("  edge [fontname=\"monospace\"];\n\n");

        let mut source_ports: BTreeMap<FieldName, DotPortEndpoint> = BTreeMap::new();
        let mut calc_input_ports: BTreeMap<FieldName, DotPortEndpoint> = BTreeMap::new();
        let mut peripheral_input_ports: BTreeMap<FieldName, DotPortEndpoint> = BTreeMap::new();
        let mut peripheral_input_node_ids = Vec::new();
        let mut peripheral_output_node_ids = Vec::new();

        // Calc nodes with input and output ports
        dot.push_str("  subgraph cluster_calcs {\n");
        dot.push_str("    label=\"calcs\";\n");
        for (calc_name, calc) in self.calcs.iter() {
            let node_id = format!("calc::{calc_name}");
            let input_names = calc.get_input_names();
            let output_names = calc.get_output_names();
            let label = dot_record_label(calc_name, &input_names, &output_names);
            let _ = writeln!(dot, "    {} [label=\"{}\"];", dot_quote_id(&node_id), label);

            for (i, input_name) in input_names.iter().enumerate() {
                calc_input_ports.insert(
                    format!("{calc_name}.{input_name}"),
                    DotPortEndpoint {
                        node_id: node_id.clone(),
                        port_id: format!("in_{i}"),
                    },
                );
            }

            for (i, output_name) in output_names.iter().enumerate() {
                source_ports.insert(
                    format!("{calc_name}.{output_name}"),
                    DotPortEndpoint {
                        node_id: node_id.clone(),
                        port_id: format!("out_{i}"),
                    },
                );
            }
        }
        dot.push_str("  }\n\n");

        // Peripheral inputs and outputs as separate nodes so that they can be ordered
        // to the left and right to make a coherent dependency flow
        dot.push_str("  subgraph cluster_peripherals {\n");
        dot.push_str("    label=\"peripherals\";\n");
        for (name, peripheral) in peripherals.iter() {
            let input_names = peripheral.input_names();
            let output_names = peripheral.output_names();
            let output_node_id = format!("periph::{name}::out");
            let input_node_id = format!("periph::{name}::in");
            peripheral_output_node_ids.push(output_node_id.clone());
            peripheral_input_node_ids.push(input_node_id.clone());

            let output_label = dot_record_label(&format!("{name} outputs"), &[], &output_names);
            let input_label = dot_record_label(&format!("{name} inputs"), &input_names, &[]);

            let _ = writeln!(
                dot,
                "    {} [label=\"{}\"];",
                dot_quote_id(&output_node_id),
                output_label
            );
            let _ = writeln!(
                dot,
                "    {} [label=\"{}\"];",
                dot_quote_id(&input_node_id),
                input_label
            );
            let _ = writeln!(
                dot,
                "    {} -> {} [style=invis, weight=100];",
                dot_quote_id(&output_node_id),
                dot_quote_id(&input_node_id)
            ); // Invisible chain ordering

            for (i, input_name) in input_names.iter().enumerate() {
                let field = format!("{name}.{input_name}");
                let endpoint = DotPortEndpoint {
                    node_id: input_node_id.clone(),
                    port_id: format!("in_{i}"),
                };
                source_ports.insert(field.clone(), endpoint.clone());
                peripheral_input_ports.insert(field, endpoint);
            }

            for (i, output_name) in output_names.iter().enumerate() {
                source_ports.insert(
                    format!("{name}.{output_name}"),
                    DotPortEndpoint {
                        node_id: output_node_id.clone(),
                        port_id: format!("out_{i}"),
                    },
                );
            }
        }
        dot.push_str("  }\n\n");

        // Set peripheral input and output node rank to push left and right
        if !peripheral_input_node_ids.is_empty() {
            dot.push_str("  subgraph peripheral_input_rank {\n");
            dot.push_str("    rank=max;\n");
            for node_id in &peripheral_input_node_ids {
                let _ = writeln!(dot, "    {};", dot_quote_id(node_id));
            }
            dot.push_str("  }\n");
        }

        if !peripheral_output_node_ids.is_empty() {
            dot.push_str("  subgraph peripheral_output_rank {\n");
            dot.push_str("    rank=min;\n");
            for node_id in &peripheral_output_node_ids {
                let _ = writeln!(dot, "    {};", dot_quote_id(node_id));
            }
            dot.push_str("  }\n");
        }
        dot.push('\n');

        // Set rank based on eval depth
        if let Ok((_, eval_depth_groups)) = self.eval_order() {
            for group in eval_depth_groups.iter().filter(|x| !x.is_empty()) {
                dot.push_str("  { rank=same;");
                for calc_name in group {
                    let _ = write!(dot, " {}", dot_quote_id(&format!("calc::{calc_name}")));
                }
                dot.push_str(" }\n");
            }
            dot.push('\n');
        }

        let mut unresolved_fields: BTreeSet<FieldName> = BTreeSet::new();
        let mut edges = Vec::new();

        // Add calc edges
        for (calc_name, calc) in self.calcs.iter() {
            let input_map = calc.get_input_map();
            for input_name in calc.get_input_names() {
                let key = format!("{calc_name}.{input_name}");
                let Some(dst) = calc_input_ports.get(&key) else {
                    continue;
                };
                let Some(src_field) = input_map.get(&input_name) else {
                    continue;
                };

                if let Some(src) = source_ports.get(src_field) {
                    edges.push(format!(
                        "  {}:{} -> {}:{};",
                        dot_quote_id(&src.node_id),
                        src.port_id,
                        dot_quote_id(&dst.node_id),
                        dst.port_id
                    ));
                } else {
                    unresolved_fields.insert(src_field.clone());
                    edges.push(format!(
                        "  {} -> {}:{};",
                        dot_quote_id(&unknown_field_node_id(src_field)),
                        dot_quote_id(&dst.node_id),
                        dst.port_id
                    ));
                }
            }
        }

        // Add peripheral edges
        for (dst_field, src_field) in self.peripheral_input_sources.iter() {
            let src_ref = if let Some(src) = source_ports.get(src_field) {
                format!("{}:{}", dot_quote_id(&src.node_id), src.port_id)
            } else {
                unresolved_fields.insert(src_field.clone());
                dot_quote_id(&unknown_field_node_id(src_field))
            };

            let dst_ref = if let Some(dst) = peripheral_input_ports.get(dst_field) {
                format!("{}:{}", dot_quote_id(&dst.node_id), dst.port_id)
            } else {
                unresolved_fields.insert(dst_field.clone());
                dot_quote_id(&unknown_field_node_id(dst_field))
            };

            edges.push(format!("  {src_ref} -> {dst_ref};"));
        }

        // Account for any leftovers (there shouldn't be any, but if there are, they won't be invisible)
        if !unresolved_fields.is_empty() {
            for field_name in unresolved_fields.iter() {
                let node_id = unknown_field_node_id(field_name);
                let _ = writeln!(
                    dot,
                    "  {} [shape=plaintext, label=\"{}\"];",
                    dot_quote_id(&node_id),
                    dot_escape_string(field_name)
                );
            }
            dot.push('\n');
        }

        for edge in edges {
            dot.push_str(&edge);
            dot.push('\n');
        }

        dot.push_str("}\n");
        dot
    }

    /// Determine the order to evaluate the calcs by traversing the calc graph
    /// and the evaluation depth of each calc. Eval depth is returned as a sequential
    /// grouping of calc names.
    pub fn eval_order(&self) -> Result<(Vec<CalcName>, Vec<Vec<CalcName>>), String> {
        let mut eval_order: Vec<CalcName> = Vec::with_capacity(self.calcs.len());
        let mut eval_depth_groups: Vec<Vec<CalcName>> = Vec::new();

        // The node name is always the first element of the name,
        // and any other `.` is not for our usage
        fn get_node_name(field_name: &str) -> CalcName {
            field_name.split(".").collect::<Vec<&str>>()[0].to_owned()
        }
        let calc_names = self.calcs.keys().cloned().collect::<Vec<CalcName>>();

        // Collate parent _calcs_ of each calc, not including the peripherals
        let mut calc_node_parents = BTreeMap::new();
        for (name, calc) in self.calcs.iter() {
            let mut calc_parents = Vec::new();

            for field_name in calc.get_input_map().values() {
                let node = get_node_name(field_name);
                if self.calcs.contains_key(&node) {
                    calc_parents.push(node);
                }
            }
            calc_node_parents.insert(name.clone(), calc_parents);
        }
        let calc_node_parents = calc_node_parents; // No longer mutable

        // Traverse the calcs, developing the evaluation order
        let mut evaluated = BTreeMap::new();
        for name in self.calcs.keys() {
            evaluated.insert(name.clone(), false);
        }

        //   While there are any calcs that have not been evaluated,
        //   evaluate any that are ready.
        let max_depth = calc_names.len() + 1;
        let mut traversal_iterations = 0;
        while evaluated.values().any(|x| !x) {
            // Check depth
            traversal_iterations += 1;
            if traversal_iterations > max_depth {
                return Err("Calc graph contains a dependency cycle.".to_string());
            }

            // Loop over calcs and find the ones that are ready to evaluate next
            let mut any_new_evaluated = false;
            let mut eval_group = Vec::new();
            for name in self.calcs.keys() {
                // Find calcs which have not been evaluated
                if !evaluated[name] {
                    // Find calcs which have no parents that have not been evaluated
                    // (which will also be true if they have no parents at all)
                    let all_parents_ready = !calc_node_parents[name]
                        .iter()
                        .any(|parent| !evaluated[parent]);

                    if all_parents_ready {
                        // Mark those nodes to evaluate next
                        eval_order.push(name.clone());
                        eval_group.push(name.clone());
                        any_new_evaluated = true;
                    }
                }
            }

            // Add this depth group
            for name in &eval_group {
                // Mark evaluated after finishing group so that we don't flatten any groups with sequential dependencies
                evaluated.insert(name.clone(), true);
            }
            eval_depth_groups.push(eval_group);

            // Check if we are stuck on a loop.
            //
            // If there are no calcs that can be evaluated, but there are some left
            // that have not been evaluated yet, this means that at least one calc
            // depends on itself.
            if !any_new_evaluated {
                return Err("Calc graph contains cyclic dependencies".to_string());
            }
        }

        Ok((eval_order, eval_depth_groups))
    }

    /// Set up calc tape and (re-)initialize individual calcs
    pub fn init(
        &mut self,
        ctx: ControllerCtx,
        peripherals: &BTreeMap<String, Box<dyn Peripheral>>,
    ) -> Result<(), String> {
        // These will be stored
        let mut peripheral_output_slices: BTreeMap<PeripheralName, Range<usize>> = BTreeMap::new();
        let mut peripheral_input_slices: BTreeMap<PeripheralName, Range<usize>> = BTreeMap::new();
        let mut peripheral_input_source_indices: Vec<(usize, usize)> = Vec::new();
        let mut peripheral_input_fields: Vec<FieldName> = Vec::new();

        let mut dispatch_names: Vec<FieldName> = Vec::new();
        let mut dispatch_indices: Vec<usize> = Vec::new();

        // Check names for collisions and formatting
        //   Collate names of peripherals and calcs
        let peripheral_names = peripherals.keys().cloned().collect::<Vec<PeripheralName>>();
        let calc_names = self.calcs.keys().cloned().collect::<Vec<CalcName>>();
        let mut node_names = Vec::new();
        node_names.extend_from_slice(&peripheral_names);
        node_names.extend_from_slice(&calc_names);
        let node_names = node_names; // No longer mutable

        //   Make sure no calc or peripheral names contain `.`
        if node_names.iter().any(|x| x.contains('.')) {
            return Err("Calc and peripheral names must not contain `.` characters".to_string());
        }

        //   Make sure there aren't any duplicate names
        if HashSet::<String>::from_iter(node_names.iter().cloned()).len() != node_names.len() {
            return Err("Peripheral names must not overlap with calc names".to_string());
        }

        // Determine order to evaluate calcs
        let eval_order = self.eval_order()?.0;

        // Build the calc and dispatch index maps using the calc order
        let mut fields_order = Vec::new();
        let mut field_index_map = BTreeMap::new();
        //    Peripheral inputs
        for (name, peripheral) in peripherals.iter() {
            // Record the contiguous slice where this data will be placed
            let start = fields_order.len();
            let end = start + peripheral.input_names().len();
            peripheral_input_slices.insert(name.clone(), start..end);

            // Populate the field order
            for (i, field) in (start..).zip(
                peripheral
                    .input_names()
                    .iter()
                    .map(|field| format!("{name}.{field}")),
            ) {
                fields_order.push(field.clone());
                field_index_map.insert(field.clone(), i);
                dispatch_names.push(field.clone());
                dispatch_indices.push(i);
                peripheral_input_fields.push(field);
            }
        }
        //    Peripheral outputs
        for (name, peripheral) in peripherals.iter() {
            // Record the contiguous slice where this data will be placed
            let start = fields_order.len();
            let end = start + peripheral.output_names().len();
            peripheral_output_slices.insert(name.clone(), start..end);

            // Populate the field order
            for (i, field) in (start..).zip(
                peripheral
                    .output_names()
                    .iter()
                    .map(|field| format!("{name}.{field}")),
            ) {
                fields_order.push(field.clone());
                field_index_map.insert(field.clone(), i);
                dispatch_names.push(field);
                dispatch_indices.push(i);
            }
        }
        //    Calcs, in eval order
        for calc_name in &eval_order {
            // Unpack
            let calc = self
                .calcs
                .get_mut(calc_name)
                .ok_or_else(|| format!("Calc `{calc_name}` missing from registry"))?;
            let input_map = calc.get_input_map();
            let save_outputs = calc.get_save_outputs();
            let input_names = &calc.get_input_names();
            let output_names = &calc.get_output_names();
            let n_outputs = output_names.len();

            // Find input indices from the part of the map that has
            // been built so far
            let mut input_indices = Vec::new();
            for input_name in input_names {
                let src_field = input_map.get(input_name).ok_or_else(|| {
                    format!("Calc `{calc_name}` missing mapping for input `{input_name}`")
                })?;
                let src_index = *field_index_map.get(src_field).ok_or_else(|| {
                    format!("Did not find field index for `{src_field}` while initializing `{calc_name}`")
                })?;
                input_indices.push(src_index);
            }

            // Set output range
            let start = fields_order.len();
            let end = start + n_outputs;
            let output_range = start..end;

            // Set output order
            for (i, output_name) in (start..).zip(output_names.iter()) {
                let output_field = format!("{calc_name}.{output_name}");
                fields_order.push(output_field.clone());
                field_index_map.insert(output_field.clone(), i);

                // Mark fields for dispatch
                if save_outputs {
                    dispatch_names.push(output_field.clone());
                    dispatch_indices.push(i);
                }
            }

            // Initialize this calc
            calc.init(ctx.clone(), input_indices, output_range)?;
        }

        // Find the indices of fields that will be given to the peripherals
        // as inputs
        for (peripheral_input_field, src_field) in &self.peripheral_input_sources {
            let dst_index = *field_index_map.get(peripheral_input_field).ok_or_else(|| {
                format!("Did not find field index for peripheral input `{peripheral_input_field}`")
            })?;
            let src_index = *field_index_map
                .get(src_field)
                .ok_or_else(|| format!("Did not find field index for source `{src_field}`"))?;
            peripheral_input_source_indices.push((dst_index, src_index));
        }

        // Track peripheral inputs that are not driven by calc outputs
        let mut manual_input_indices = BTreeMap::new();
        let mut manual_input_names = Vec::new();
        for field in peripheral_input_fields {
            if self.peripheral_input_sources.contains_key(&field) {
                continue;
            }
            let index = *field_index_map.get(&field).ok_or_else(|| {
                format!("Did not find field index for peripheral input `{field}`")
            })?;
            manual_input_indices.insert(field.clone(), index);
            manual_input_names.push(field);
        }

        // Initialize the calc tape
        let calc_tape: Vec<f64> = vec![0.0_f64; fields_order.len()];

        // Take new internal state
        self.state = CalcOrchestratorState {
            calc_tape,
            eval_order,
            dispatch_names,
            dispatch_indices,
            peripheral_output_slices,
            peripheral_input_slices,
            peripheral_input_source_indices,
            manual_input_indices,
        };
        Ok(())
    }

    /// Clear state to reset for another run
    pub fn terminate(&mut self) -> Result<(), String> {
        self.state = CalcOrchestratorState::default();
        let mut errors = Vec::new();
        for (name, calc) in self.calcs.iter_mut() {
            if let Err(e) = calc.terminate() {
                errors.push(format!("\n  {name}: {e}"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("Failed to terminate calcs: {}", errors.join("")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calc::Affine;
    use crate::peripheral::AnalogIRev3;

    #[test]
    fn graphviz_dot_uses_node_ports() {
        let mut orchestrator = CalcOrchestrator::default();
        orchestrator.add_calc("c0", Affine::new("p.ain0".to_owned(), 1.0, 0.0, true));
        orchestrator.add_calc("c1", Affine::new("c0.y".to_owned(), 2.0, 0.0, true));
        orchestrator.set_peripheral_input_source("p.pwm0_duty", "c1.y");

        let peripherals: BTreeMap<String, Box<dyn Peripheral>> = BTreeMap::from([(
            "p".to_owned(),
            Box::new(AnalogIRev3 { serial_number: 1 }) as _,
        )]);

        let dot = orchestrator.graphviz_dot(&peripherals);

        assert!(dot.contains("\"periph::p::out\":out_0 -> \"calc::c0\":in_0;"));
        assert!(dot.contains("\"calc::c0\":out_0 -> \"calc::c1\":in_0;"));
        assert!(dot.contains("\"calc::c1\":out_0 -> \"periph::p::in\":in_0;"));
    }
}
