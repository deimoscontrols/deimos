//! Calculations that are run at each cycle during operation.

use std::collections::HashSet;
use std::iter::Iterator;
use std::{collections::BTreeMap, ops::Range};

use serde::{Deserialize, Serialize};

use crate::ControllerCtx;
use crate::{calc::*, peripheral::Peripheral};
use deimos_shared::states::OperatingMetrics;

/// Internal state of calc orchestrator
#[derive(Default)]
struct OrchestratorState {
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
}

/// Calculations that are run at each cycle during operation.
/// Because the results of the calculations are needed at each cycle,
/// they are run synchronously, and must complete before the next cycle.
///
/// `Calc` objects are registered with the `Orchestrator` and serialized with the controller.
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
pub struct Orchestrator {
    calcs: BTreeMap<CalcName, Box<dyn Calc>>,
    peripheral_input_sources: BTreeMap<PeripheralInputName, FieldName>,

    #[serde(skip)]
    state: OrchestratorState,
}

impl Orchestrator {
    /// Synchronize calc state using latest outputs from peripherals
    /// by running each calc in the order determined during init.
    ///
    /// # Panics
    /// * If eval() is called before init()
    /// * If evaluation of individual calcs panics
    pub fn eval(&mut self) {
        // Evaluate calcs in order
        for name in self.state.eval_order.iter() {
            self.calcs
                .get_mut(name)
                .unwrap()
                .eval(&mut self.state.calc_tape);
        }

        // Populate peripheral inputs
        for (dst_index, src_index) in self.state.peripheral_input_source_indices.iter().copied() {
            self.state.calc_tape[dst_index] = self.state.calc_tape[src_index];
        }
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
    pub fn provide_dispatcher_outputs(&self, mut f: impl FnMut(&mut dyn Iterator<Item = f64>)) {
        let inds = self.state.dispatch_indices.iter();
        let mut vals = inds.map(|&i| self.state.calc_tape[i]);

        f(&mut vals);
    }

    /// Get names of fields marked to dispatch
    pub fn get_dispatch_names(&self) -> Vec<String> {
        self.state.dispatch_names.clone()
    }

    /// Add a calc
    ///
    /// # Panics
    /// * If a calc with this name already exists
    pub fn add_calc(&mut self, name: &str, calc: Box<dyn Calc>) {
        let name = name.to_owned();
        if self.calcs.contains_key(&name) {
            panic!("A calc with named `{name}` already exists.");
        }
        self.calcs.insert(name, calc);
    }

    /// Add multiple calcs
    ///
    /// # Panics
    /// * If, for any calc to add, a calc with this name already exists
    pub fn add_calcs(&mut self, mut calcs: BTreeMap<String, Box<dyn Calc>>) {
        while let Some((name, calc)) = calcs.pop_first() {
            self.add_calc(&name, calc);
        }
    }

    /// Remove all calcs and peripheral input sources
    pub fn clear_calcs(&mut self) {
        self.calcs.clear();
        self.peripheral_input_sources.clear();
    }

    /// Add a calc, replacing an entry if it already exists
    pub fn replace_calc(&mut self, name: &str, calc: Box<dyn Calc>) {
        let name = name.to_owned();
        self.calcs.insert(name, calc);
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

    /// Determine the order to evaluate the calcs by traversing the calc graph
    /// and the evaluation depth of each calc. Eval depth is returned as a sequential
    /// grouping of calc names.
    pub fn eval_order(&self) -> (Vec<CalcName>, Vec<Vec<CalcName>>) {
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
        for name in self.calcs.keys().cloned() {
            evaluated.insert(name.clone(), false);
        }

        //   While there are any calcs that have not been evaluated,
        //   evaluate any that are ready.
        let max_depth = calc_names.len() + 1;
        let mut traversal_iterations = 0;
        while evaluated.values().any(|x| !x) {
            // Check depth
            traversal_iterations += 1;
            assert!(
                traversal_iterations <= max_depth,
                "Calc graph contains a dependency cycle."
            );

            // Loop over calcs and find the ones that are ready to evaluate next
            let mut any_new_evaluated = false;
            let mut eval_group = Vec::new();
            for name in self.calcs.keys().cloned() {
                // Find calcs which have not been evaluated
                if !evaluated[&name] {
                    // Find calcs which have no parents that have not been evaluated
                    // (which will also be true if they have no parents at all)
                    let all_parents_ready = !calc_node_parents[&name]
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
                panic!("Calc graph contains cyclic dependencies");
            }
        }

        (eval_order, eval_depth_groups)
    }

    /// Set up calc tape and (re-)initialize individual calcs
    pub fn init(
        &mut self,
        ctx: ControllerCtx,
        peripherals: &BTreeMap<String, Box<dyn Peripheral>>,
    ) {
        // These will be stored
        let mut peripheral_output_slices: BTreeMap<PeripheralName, Range<usize>> = BTreeMap::new();
        let mut peripheral_input_slices: BTreeMap<PeripheralName, Range<usize>> = BTreeMap::new();
        let mut peripheral_input_source_indices: Vec<(usize, usize)> = Vec::new();

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
        assert!(
            !node_names.iter().any(|x| x.contains(".")),
            "Calc and peripheral names must not contain `.` characters"
        );

        //   Make sure there aren't any duplicate names
        assert_eq!(
            HashSet::<String>::from_iter(node_names.iter().cloned()).len(),
            node_names.len(),
            "Peripheral names must not overlap with calc names"
        );

        // Determine order to evaluate calcs
        let eval_order = self.eval_order().0;

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
            let mut i = start;
            for field in peripheral
                .input_names()
                .iter()
                .map(|field| format!("{name}.{field}"))
            {
                fields_order.push(field.clone());
                field_index_map.insert(field.clone(), i);
                dispatch_names.push(field);
                dispatch_indices.push(i);
                i += 1;
            }
        }
        //    Peripheral outputs
        for (name, peripheral) in peripherals.iter() {
            // Record the contiguous slice where this data will be placed
            let start = fields_order.len();
            let end = start + peripheral.output_names().len();
            peripheral_output_slices.insert(name.clone(), start..end);

            // Populate the field order
            let mut i = start;
            for field in peripheral
                .output_names()
                .iter()
                .map(|field| format!("{name}.{field}"))
            {
                fields_order.push(field.clone());
                field_index_map.insert(field.clone(), i);
                dispatch_names.push(field);
                dispatch_indices.push(i);
                i += 1;
            }
        }
        //    Calcs, in eval order
        for calc_name in &eval_order {
            // Unpack
            let calc = self.calcs.get_mut(calc_name).unwrap();
            let input_map = calc.get_input_map();
            let save_outputs = calc.get_save_outputs();
            let input_names = &calc.get_input_names();
            let output_names = &calc.get_output_names();
            let n_outputs = output_names.len();

            // Find input indices from the part of the map that has
            // been built so far
            let mut input_indices = Vec::new();
            for input_name in input_names {
                let src_field = &input_map[input_name];
                let src_index = *field_index_map
                    .get(src_field)
                    .unwrap_or_else(|| panic!("Did not find field index for {src_field}"));
                input_indices.push(src_index);
            }

            // Set output range
            let start = fields_order.len();
            let end = start + n_outputs;
            let output_range = start..end;

            // Set output order
            let mut i = start;
            for output_name in output_names {
                let output_field = format!("{calc_name}.{output_name}");
                fields_order.push(output_field.clone());
                field_index_map.insert(output_field.clone(), i);

                // Mark fields for dispatch
                if save_outputs {
                    dispatch_names.push(output_field.clone());
                    dispatch_indices.push(i);
                }

                i += 1;
            }

            // Initialize this calc
            calc.init(ctx.clone(), input_indices, output_range)
        }

        // Find the indices of fields that will be given to the peripherals
        // as inputs
        for (peripheral_input_field, src_field) in &self.peripheral_input_sources {
            let dst_index = field_index_map[peripheral_input_field];
            let src_index = field_index_map[src_field];
            peripheral_input_source_indices.push((dst_index, src_index));
        }

        // Initialize the calc tape
        let calc_tape: Vec<f64> = vec![0.0_f64; fields_order.len()];

        // Take new internal state
        self.state = OrchestratorState {
            calc_tape,
            eval_order,
            dispatch_names,
            dispatch_indices,
            peripheral_output_slices,
            peripheral_input_slices,
            peripheral_input_source_indices,
        };
    }

    /// Clear state to reset for another run
    pub fn terminate(&mut self) {
        self.state = OrchestratorState::default();
        self.calcs.values_mut().for_each(|c| c.terminate());
    }
}
