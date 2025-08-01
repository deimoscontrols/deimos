use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Debug;
use std::rc::Rc;

use canvas::Event;
use iced::alignment::Horizontal;
use iced::border::Radius;
use iced::keyboard::key::{Code, Named, Physical};
use iced::keyboard::{Key, Modifiers};
use iced::mouse::{Button, Cursor};
use iced::widget::{
    Column, Row, button,
    canvas::{self, Canvas, Frame, Geometry, Path, Program, Text},
    horizontal_rule,
};
use iced::{Color, Element, Font, Length, Point, Rectangle, Renderer, Theme, Vector};

// We need StableGraph, which preserves indices through deletions,
// in order to handle links between composite nodes' subnodes
// (the input and output nodes of a Peripheral).
use petgraph::stable_graph::{NodeIndex, StableGraph};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};

use petgraph::Direction;
use serde::{Deserialize, Serialize};

use bimap::BiBTreeMap;

use deimos::{self, Controller, calc::Calc, peripheral::Peripheral};

const NODE_WIDTH_PX: f32 = 250.0;
const PORT_STRIDE_PX: f32 = 20.0;
const LEFT_PAD_PX: f32 = 10.0;

const BGCOLOR: Color = iced::Color::WHITE;
const NODECOLOR: Color = iced::Color::from_rgb(0.9, 0.9, 0.9);
const EDGECOLOR: Color = Color::BLACK;
const SELECTION_COLOR: Color = iced::Color::from_rgb(0.2, 0.8, 0.2);
const TEXT_COLOR: Color = Color::BLACK;

const FONT: Font = Font {
    weight: iced::font::Weight::Bold,
    ..Font::MONOSPACE
};

pub fn main() -> iced::Result {
    iced::application("Deimos Editor", NodeEditor::update, NodeEditor::view)
        .theme(|_| Theme::Light)
        .centered()
        .antialiasing(true)
        .default_font(iced::Font::MONOSPACE)
        .resizable(true)
        .run()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Port {
    /// Relative to top of node frame
    offset_px: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum NodeKind {
    Calc {
        inner: Box<dyn Calc>,
    },
    Peripheral {
        inner: Box<dyn Peripheral>,
        partner: NodeIndex,
        is_input_side: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct NodeData {
    name: String,
    kind: NodeKind,
    inputs: BiBTreeMap<String, usize>,
    outputs: BiBTreeMap<String, usize>,
    position: (f32, f32),
    input_ports: Vec<Port>,
    output_ports: Vec<Port>,
    size: (f32, f32),
}

impl NodeData {
    pub fn new(name: String, kind: NodeKind, position: (f32, f32)) -> NodeData {
        let (inputs, outputs) = match &kind {
            NodeKind::Calc { inner } => (inner.get_input_names(), inner.get_output_names()),
            NodeKind::Peripheral {
                inner,
                partner: _,
                is_input_side: false,
            } => (vec![], inner.output_names()),
            NodeKind::Peripheral {
                inner,
                partner: _,
                is_input_side: true,
            } => (inner.input_names(), vec![]),
        };

        let mut input_map = BiBTreeMap::new();
        let mut output_map = BiBTreeMap::new();

        let mut input_ports = Vec::with_capacity(inputs.len());
        let mut output_ports = Vec::with_capacity(outputs.len());

        // Determine y-position of each port within the node widget
        let mut inp_offs = 0.0_f32;
        inputs.iter().enumerate().for_each(|(i, n)| {
            inp_offs += PORT_STRIDE_PX;
            input_map.insert(n.clone(), i);
            input_ports.push(Port {
                offset_px: inp_offs,
            })
        });

        let mut out_offs = 0.0_f32;
        outputs.iter().enumerate().for_each(|(i, n)| {
            out_offs += PORT_STRIDE_PX;
            output_map.insert(n.clone(), i);
            output_ports.push(Port {
                offset_px: out_offs,
            })
        });

        // Determine width and height of node widget
        let width = NODE_WIDTH_PX;
        let height = inp_offs.max(out_offs) + PORT_STRIDE_PX;
        let size = (width, height);

        // Pin peripheral output nodes to the left side
        let mut position = position;
        if let NodeKind::Peripheral {
            inner: _,
            partner: _,
            is_input_side: false,
        } = &kind
        {
            position.0 = LEFT_PAD_PX;
        };

        Self {
            name,
            kind,
            inputs: input_map,
            outputs: output_map,
            position,
            input_ports,
            output_ports,
            size,
        }
    }

    pub fn size(&self) -> iced::Size {
        iced::Size::new(self.size.0, self.size.1)
    }

    pub fn position(&self) -> Point {
        Point {
            x: self.position.0,
            y: self.position.1,
        }
    }

    pub fn get_input_port(&self, name: &str) -> &Port {
        let ind = self
            .inputs
            .get_by_left(name)
            .unwrap_or_else(|| panic!("Node `{}` missing input port `{}`", self.name, name));
        &self.input_ports[*ind]
    }

    pub fn get_output_port(&self, name: &str) -> &Port {
        let ind = self
            .outputs
            .get_by_left(name)
            .unwrap_or_else(|| panic!("Node `{:?}` missing output port `{}`", self, name));
        &self.output_ports[*ind]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct EdgeData {
    from_port: String,
    to_port: String,
}

#[derive(Debug, Clone)]
enum CanvasMessage {
    MoveNode(NodeIndex, (f32, f32)),
    AddEdge(NodeIndex, NodeIndex, EdgeData),
    RemoveEdge(NodeIndex, NodeIndex, EdgeData),
    #[allow(dead_code)]
    AddNode(NodeData),
    RemoveNode(NodeIndex),
    Checkpoint,
    Undo,
    Redo,
}

#[derive(Clone, Debug)]
enum MenuMessage {
    Load,
    // Save,
    // SaveAs,
    // AutoLayout,
}

#[derive(Clone, Debug)]
enum Message {
    Canvas(CanvasMessage),
    Menu(MenuMessage),
}

#[derive(Default)]
struct NodeEditor {
    controller: Option<Controller>,

    /// Graph that never reuses an index
    graph: Rc<RefCell<StableGraph<NodeData, EdgeData>>>,

    /// Support getting nodes by name
    name_index_map: BiBTreeMap<String, NodeIndex>,

    edit_log: Vec<String>,
    redo_log: Vec<String>,
}

impl NodeEditor {
    fn update(state: &mut Self, message: Message) {
        match message {
            Message::Canvas(msg) => match msg {
                CanvasMessage::MoveNode(node_id, delta) => {
                    let mut graph = state.graph.borrow_mut();

                    let partner_to_move = if let Some(node) = graph.node_weight_mut(node_id) {
                        match &node.kind {
                            NodeKind::Calc { inner: _ } => {
                                // Calc nodes can move anywhere
                                node.position.0 += delta.0;
                                node.position.1 += delta.1;
                                None
                            }
                            NodeKind::Peripheral {
                                inner: _,
                                partner,
                                is_input_side,
                            } => {
                                if !is_input_side {
                                    // Pin peripheral outputs to left side to enforce left-to-right flow
                                    node.position.0 = LEFT_PAD_PX;
                                } else {
                                    node.position.0 += delta.0;
                                }

                                node.position.1 += delta.1;

                                Some((*partner, node.position.1))
                            }
                        }
                    } else {
                        None
                    };

                    // Maintain y-alignment for peripheral partner nodes
                    if let Some((partner, y)) = partner_to_move {
                        if let Some(partner_node) = graph.node_weight_mut(partner) {
                            partner_node.position.1 = y;
                        }
                    }
                }
                CanvasMessage::AddEdge(from, to, data) => {
                    // Check if the target port already has an input
                    let to_port = data.to_port.clone();
                    if !state
                        .graph
                        .borrow()
                        .edges_directed(to, Direction::Incoming)
                        .any(|x| x.weight().to_port == to_port)
                    {
                        state.graph.borrow_mut().add_edge(from, to, data);
                        state.checkpoint();
                    }
                }
                CanvasMessage::RemoveEdge(from, to, data) => {
                    let mut index = None;
                    for edge in state.graph.borrow().edges_connecting(from, to) {
                        if edge.weight() == &data {
                            index = Some(edge.id());
                        }
                    }

                    if let Some(edge_index) = index {
                        state.graph.borrow_mut().remove_edge(edge_index);
                        state.checkpoint();
                    }
                }
                CanvasMessage::AddNode(data) => {
                    state.graph.borrow_mut().add_node(data);
                    state.checkpoint();
                }
                CanvasMessage::RemoveNode(node_id) => {
                    state.graph.borrow_mut().remove_node(node_id);
                    state.checkpoint();
                }
                CanvasMessage::Checkpoint => state.checkpoint(),
                CanvasMessage::Undo => state.undo(),
                CanvasMessage::Redo => state.redo(),
            },
            Message::Menu(menu_msg) => match menu_msg {
                MenuMessage::Load => {
                    let f = rfd::FileDialog::new()
                        .add_filter("JSON Files", &["json"])
                        .set_title("Choose a file")
                        .set_directory("./")
                        .pick_file()
                        .map(|path| path.display().to_string());

                    if let Some(path) = f {
                        let res = state.load(&std::path::PathBuf::from(path));
                        if let Err(x) = res {
                            println!("{x}")
                        }
                    }
                } // MenuMessage::Save => {}
                  // MenuMessage::SaveAs => {}
                  // MenuMessage::AutoLayout => {}
            },
            // x => println!("Unhandled message: {x:?}"),
        };
    }

    fn view(state: &Self) -> Element<Message> {
        let canvas = Canvas::new(EditorCanvas {
            _v: core::marker::PhantomData,
            graph: state.graph.clone(),
        })
        .width(Length::Fill)
        .height(Length::Fill);

        Column::new()
            // Top bar
            .push(
                Row::new().push(button("Load").on_press(Message::Menu(MenuMessage::Load))), // .push(button("Save").on_press(Message::Menu(MenuMessage::Save)))
                                                                                            // .push(button("Save As").on_press(Message::Menu(MenuMessage::SaveAs)))
                                                                                            // .push(button("Autolayout").on_press(Message::Menu(MenuMessage::AutoLayout))),
            )
            .push(horizontal_rule(0.0))
            // Node editor
            .push(canvas)
            .into()
    }

    /// Save the full state of the graph
    fn checkpoint(&mut self) {
        let entry = serde_json::to_string(&self.graph).unwrap();
        self.edit_log.push(entry);
        if self.edit_log.len() > 30 {
            let _ = self.edit_log.pop();
        }

        // Clear redo, which will no longer be valid after other edits
        self.redo_log.clear();
    }

    /// Move graph to previous checkpoint and place this state in the redo log
    fn undo(&mut self) {
        // Move the latest state to the redo log
        if self.edit_log.len() > 1 {
            if let Some(latest) = self.edit_log.pop() {
                self.redo_log.push(latest);
            }
        }

        // Reset to the previous state
        if let Some(previous) = self.edit_log.last() {
            self.graph = serde_json::from_str(previous).unwrap();
        }
    }

    /// Reset graph to next state in redo log
    /// and move that checkpoint back to the edit log
    fn redo(&mut self) {
        if let Some(next) = self.redo_log.pop() {
            self.graph = serde_json::from_str(&next).unwrap();
            self.edit_log.push(next);
        }
    }

    /// Load a controller from disk
    fn load(&mut self, path: &std::path::Path) -> Result<(), String> {
        // Load from disk
        let s = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to load file from {path:?}: {e}"))?;
        let c: Controller = serde_json::from_str(&s)
            .map_err(|e| format!("Failed to load file from {path:?}: {e}"))?;

        // Extract order and depth of evaluation of calcs
        let (eval_order, _eval_depth_groups) = c.orchestrator().eval_order();

        // Update graph
        self.graph.borrow_mut().clear();

        //   Add peripheral nodes without edges
        for (name, p) in c.peripherals().iter() {
            self.add_peripheral(name, p);
        }

        //   Add calc nodes with edges
        for name in &eval_order {
            self.add_calc(name, &c.calcs()[name]);
        }

        //   Add peripheral input edges
        for (to, from) in c.peripheral_input_sources() {
            let (from_node, from_port) = self
                .get_output_port_by_name(from)
                .ok_or(format!("Did not find peripheral input source `{from}`"))?;
            let (to_node, to_port) = self
                .get_input_port_by_name(to)
                .ok_or(format!("Did not find peripheral input `{to}`"))?;
            let data = EdgeData { from_port, to_port };
            self.graph.borrow_mut().add_edge(from_node, to_node, data);
        }

        //   Associate Controller
        self.controller = Some(c);

        //   Set node layout
        self.autolayout()?;

        Ok(())
    }

    /// Get the calc node indices that are connected to this peripheral
    fn get_peripheral_connected(&self, name: &str) -> Result<Vec<NodeIndex>, String> {
        let mut nodes = Vec::<NodeIndex>::new();

        // Find the peripheral output node
        let &index = self.name_index_map.get_by_left(name).unwrap();
        let node = &self.graph.borrow()[index];
        let partner_index = match &node.kind {
            NodeKind::Peripheral {
                inner: _,
                partner,
                is_input_side: _,
            } => partner,
            _ => {
                return Err(format!(
                    "Expected a Peripheral named `{name}`, found a Calc"
                ));
            }
        };

        // Get everything connected both to the input and output nodes of this peripheral
        for index in [index, *partner_index] {
            // Get connected nodes, ignoring peripheral nodes
            let mut new: HashSet<NodeIndex> = HashSet::from_iter(
                self.graph
                    .borrow()
                    .neighbors_undirected(index)
                    .filter(|&x| match &self.graph.borrow()[x].kind {
                        NodeKind::Calc { inner: _ } => true,
                        _ => false,
                    }),
            );
            let mut visited: HashSet<NodeIndex> = HashSet::new();

            while !new.is_empty() {
                nodes.extend(new.iter());
                let n = new.len();
                new.clear();
                nodes[nodes.len() - n..].iter().for_each(|&i| {
                    new.extend(
                        self.graph
                            .borrow()
                            .neighbors_undirected(i)
                            .filter(|&x| match &self.graph.borrow()[x].kind {
                                NodeKind::Calc { inner: _ } => true,
                                _ => false,
                            })
                            .filter(|x| !visited.contains(x)),
                    );
                });
                visited.extend(new.iter());
            }
        }

        Ok(nodes)
    }

    /// Set node layout based on associated Controller
    fn autolayout(&mut self) -> Result<(), String> {
        if let Some(c) = &self.controller {
            let hpad = 10.0;
            let wpad = 200.0;

            // Get the evaluation depth of each calc
            let (_eval_order, eval_depth_groups) = c.orchestrator().eval_order();

            // Set calc node positions
            let xbase = wpad + NODE_WIDTH_PX + LEFT_PAD_PX;
            let mut ybase = hpad;
            let mut y = ybase;
            let mut ymax = ybase; // Maximum y-value seen so far
            let mut visited = HashSet::new();

            for pname in c.peripherals().keys() {
                let connected: HashSet<NodeIndex> =
                    HashSet::from_iter(self.get_peripheral_connected(pname)?);
                let mut x = xbase;
                for group in &eval_depth_groups {
                    y = ybase;
                    for name in group {
                        let index = self
                            .name_index_map
                            .get_by_left(name)
                            .ok_or(format!("Missing node `{name}` for layout"))?;

                        if !connected.contains(index) || visited.contains(index) {
                            continue;
                        }

                        let height = self
                            .graph
                            .borrow()
                            .node_weight(*index)
                            .unwrap()
                            .size()
                            .height;
                        self.graph
                            .borrow_mut()
                            .node_weight_mut(*index)
                            .unwrap()
                            .position = (x, y);
                        visited.insert(index);

                        y += hpad + height;
                        ymax = ymax.max(y);
                    }
                    x += wpad + NODE_WIDTH_PX;
                    y += hpad * 5.0;
                    ymax = ymax.max(y);
                }

                // Set peripheral positions
                let &index = self
                    .name_index_map
                    .get_by_left(pname)
                    .ok_or(format!("Missing node `{pname}` for layout"))?;
                let (&partner_index, &is_input_side) =
                    match &self.graph.borrow().node_weight(index).unwrap().kind {
                        NodeKind::Peripheral {
                            inner: _,
                            partner,
                            is_input_side,
                        } => (partner, is_input_side),
                        _ => {
                            return Err(format!(
                                "Node `{pname}` expected to be a Peripheral, but is a Calc."
                            ));
                        }
                    };

                // Height allotted for this peripheral is the max of the input or output height
                let h1 = self
                    .graph
                    .borrow()
                    .node_weight(index)
                    .unwrap()
                    .size()
                    .height;
                let h2 = self
                    .graph
                    .borrow()
                    .node_weight(partner_index)
                    .unwrap()
                    .size()
                    .height;
                let h = h1.max(h2);

                for (i, side) in [(index, !is_input_side), (partner_index, is_input_side)] {
                    let x_this_side = match side {
                        true => LEFT_PAD_PX,
                        false => x,
                    };

                    self.graph.borrow_mut().node_weight_mut(i).unwrap().position =
                        (x_this_side, ybase)
                }

                y += hpad + h;
                ymax = ymax.max(y);

                ybase = ymax;
            }

            Ok(())
        } else {
            Err("Unable to perform autolayout without associated Controller.".into())
        }
    }

    /// Add a peripheral node (input and output node pair) to the graph. Does not add edges.
    fn add_peripheral(&mut self, name: &str, p: &Box<dyn Peripheral>) -> (NodeIndex, NodeIndex) {
        let a = self.graph.borrow_mut().add_node(NodeData::new(
            name.to_string(),
            NodeKind::Peripheral {
                inner: p.clone(),
                partner: NodeIndex::default(),
                is_input_side: true,
            },
            (100.0, 0.0),
        ));

        let b = self.graph.borrow_mut().add_node(NodeData::new(
            name.to_string(),
            NodeKind::Peripheral {
                inner: p.clone(),
                partner: a,
                is_input_side: false,
            },
            (0.0, 0.0),
        ));

        //   Link input and output parts of peripheral
        self.graph.borrow_mut().node_weight_mut(a).unwrap().kind = NodeKind::Peripheral {
            inner: p.clone(),
            partner: b,
            is_input_side: true,
        };

        //   Store just the input side in the index map
        self.name_index_map.insert(name.to_string(), a);

        (a, b)
    }

    /// Add a calc node to the graph along with its incoming edges.
    fn add_calc(&mut self, name: &str, c: &Box<dyn Calc>) -> NodeIndex {
        // Update graph
        //   Add calc node
        let a = self.graph.borrow_mut().add_node(NodeData::new(
            name.into(),
            NodeKind::Calc { inner: c.clone() },
            (0.0, 0.0),
        ));
        //   Add input edges
        for (inp, src) in c.get_input_map().iter() {
            let (snode, sport) = self.get_output_port_by_name(src).unwrap();
            let data = EdgeData {
                from_port: sport,
                to_port: inp.clone(),
            };
            self.graph.borrow_mut().add_edge(snode, a, data);
        }

        // Update index map
        self.name_index_map.insert(name.into(), a);

        a
    }

    /// Get a port and its node by full name (like `node.port`)
    fn get_input_port_by_name(&mut self, name: &str) -> Option<(NodeIndex, String)> {
        let (node_name, port_name) = name.split_once(".")?;
        let node_index = *self.name_index_map.get_by_left(node_name)?;

        // This might be a composite node - check the partner node
        match &self.graph.borrow().node_weight(node_index)?.kind {
            NodeKind::Peripheral {
                inner: _,
                partner,
                is_input_side,
            } => {
                let node_index = if *is_input_side { node_index } else { *partner };
                Some((node_index, port_name.to_string()))
            }
            NodeKind::Calc { inner: _ } => Some((node_index, port_name.to_string())),
        }
    }

    /// Get a port and its node by full name (like `node.port`)
    fn get_output_port_by_name(&mut self, name: &str) -> Option<(NodeIndex, String)> {
        let (node_name, port_name) = name.split_once(".")?;
        let node_index = *self.name_index_map.get_by_left(node_name)?;

        // This might be a composite node - check the partner node
        match &self.graph.borrow().node_weight(node_index)?.kind {
            NodeKind::Peripheral {
                inner: _,
                partner,
                is_input_side,
            } => {
                let node_index = if !*is_input_side {
                    node_index
                } else {
                    *partner
                };
                Some((node_index, port_name.to_string()))
            }
            NodeKind::Calc { inner: _ } => Some((node_index, port_name.to_string())),
        }
    }
}

/// Ongoing actions that are mutually exclusive
#[derive(Default, PartialEq, Eq)]
enum ExclusiveActionCtx {
    #[default]
    None,
    NodeSelected(NodeIndex),
    EdgeSelected(NodeIndex, NodeIndex, EdgeData),
    ConnectingFromPort((NodeIndex, usize)),
}

struct EditorState {
    initialized: bool,
    pan: Vector,
    zoom: f32,
    panning: bool,
    dragging_node: Option<NodeIndex>,
    last_cursor_position: Option<Point>,
    action_ctx: ExclusiveActionCtx,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            initialized: false,
            pan: Vector::default(),
            zoom: 1.0, // This is why we need a default
            panning: false,
            dragging_node: None,
            last_cursor_position: None,
            action_ctx: ExclusiveActionCtx::None,
        }
    }
}

struct EditorCanvas<'a> {
    _v: core::marker::PhantomData<&'a usize>,
    graph: Rc<RefCell<StableGraph<NodeData, EdgeData>>>,
}

impl Program<Message> for EditorCanvas<'_> {
    type State = EditorState;

    fn draw(
        &self,
        state: &EditorState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        // Background
        frame.fill_rectangle(Point::ORIGIN, frame.size(), BGCOLOR);

        let zoom = state.zoom.max(0.01);

        frame.translate(state.pan);
        frame.scale(zoom);

        // Pan/zoom transform
        let t = |p: Point| (p - state.pan) * iced::Transformation::scale(1.0 / zoom);
        let cursor_pos = cursor.position_in(bounds).unwrap_or_default();
        let cursor_pos = t(cursor_pos);

        let graph = self.graph.borrow();

        // Draw edges
        for edge in graph.edge_references() {
            let from = &graph[edge.source()];
            let to = &graph[edge.target()];
            let from_port_name = &edge.weight().from_port;
            let to_port_name = &edge.weight().to_port;
            let from_port = from.get_output_port(from_port_name);
            let to_port = to.get_input_port(to_port_name);

            let from_pos = Point::new(
                from.position.0 + from.size().width,
                from.position.1 + from_port.offset_px,
            );
            let to_pos = Point::new(to.position.0, to.position.1 + to_port.offset_px);
            let ctrl1 = Point::new(from_pos.x + 30.0, from_pos.y);
            let ctrl2 = Point::new(to_pos.x - 30.0, to_pos.y);

            // Set a control point at the midpoint so that simple midpoint selection logic can work
            let delta = to_pos - from_pos;
            let mid = Point::new(from_pos.x + delta.x / 2.0, from_pos.y + delta.y / 2.0);

            let path = Path::new(|builder| {
                builder.move_to(from_pos);
                builder.bezier_curve_to(from_pos, ctrl1, mid);
                builder.bezier_curve_to(mid, ctrl2, to_pos);
            });

            let is_selected = ExclusiveActionCtx::EdgeSelected(
                edge.source(),
                edge.target(),
                edge.weight().clone(),
            ) == state.action_ctx;
            let color = if is_selected {
                SELECTION_COLOR
            } else {
                EDGECOLOR
            };

            frame.stroke(
                &path,
                canvas::Stroke::default().with_color(color).with_width(2.0),
            );
        }

        // Draw nodes
        for node_idx in graph.node_indices() {
            let node = &graph[node_idx];

            // Border
            let rect = Path::rounded_rectangle(node.position(), node.size(), Radius::new(5.0));
            let color = if ExclusiveActionCtx::NodeSelected(node_idx) == state.action_ctx {
                SELECTION_COLOR // Highlight selected
            } else {
                EDGECOLOR
            };
            frame.fill(&rect, NODECOLOR);
            frame.stroke(&rect, canvas::Stroke::default().with_color(color));

            // Label
            frame.fill_text(Text {
                content: node.name.clone(),
                position: Point::new(node.position.0 + 6.0, node.position.1 + 8.0),
                color: TEXT_COLOR,
                size: 16.0.into(),
                font: FONT,
                ..Default::default()
            });

            // Output ports
            for (i, port_name) in node.outputs.left_values().enumerate() {
                let port = &node.output_ports[i];
                let port_pos = Point::new(
                    node.position.0 + node.size().width,
                    node.position.1 + port.offset_px,
                );
                let port_circle = Path::circle(port_pos, 4.0);
                frame.fill(&port_circle, EDGECOLOR);
                frame.fill_text(Text {
                    content: port_name.clone(),
                    position: Point::new(port_pos.x + 6.0, port_pos.y - 8.0),
                    color: TEXT_COLOR,
                    size: 12.0.into(),
                    font: FONT,
                    ..Default::default()
                });
            }

            // Input ports
            for (i, port_name) in node.inputs.left_values().enumerate() {
                let port = &node.input_ports[i];
                let port_pos = Point::new(node.position.0, node.position.1 + port.offset_px);
                let port_circle = Path::circle(port_pos, 4.0);
                frame.fill(&port_circle, EDGECOLOR);
                frame.fill_text(Text {
                    content: port_name.clone(),
                    position: Point::new(port_pos.x - 8.0, port_pos.y - 8.0),
                    color: TEXT_COLOR,
                    size: 12.0.into(),
                    font: FONT,
                    horizontal_alignment: Horizontal::Right,
                    ..Default::default()
                });
            }
        }

        // Draw in-progress connection
        if let ExclusiveActionCtx::ConnectingFromPort((from_idx, port_idx)) = &state.action_ctx {
            let node = &graph[*from_idx];
            let from_pos = Point::new(
                node.position.0 + node.size().width,
                node.position.1 + &node.output_ports[*port_idx].offset_px,
            );
            let to_pos = cursor_pos;

            // Set a control point at the midpoint so that simple midpoint selection logic can work
            let ctrl1 = Point::new(from_pos.x + 30.0, from_pos.y);
            let ctrl2 = Point::new(to_pos.x - 30.0, to_pos.y);

            // Set a control point at the midpoint so that simple midpoint selection logic can work
            let delta = to_pos - from_pos;
            let mid = Point::new(from_pos.x + delta.x / 2.0, from_pos.y + delta.y / 2.0);

            let path = Path::new(|builder| {
                builder.move_to(from_pos);
                builder.bezier_curve_to(from_pos, ctrl1, mid);
                builder.bezier_curve_to(mid, ctrl2, to_pos);
            });
            frame.stroke(
                &path,
                canvas::Stroke::default()
                    .with_color(SELECTION_COLOR)
                    .with_width(2.0),
            );
        }

        vec![frame.into_geometry()]
    }

    fn update(
        &self,
        state: &mut EditorState,
        event: Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        // Make initial checkpoint
        if !state.initialized {
            state.initialized = true;
            let msg = Some(Message::Canvas(CanvasMessage::Checkpoint));
            return (canvas::event::Status::Captured, msg);
        }

        // Make sure we don't divide by zero later when we calculate the zoom scaling
        if state.zoom <= 0.0 {
            state.zoom = 1.0;
        }

        // Pan/zoom transform
        let t = |p: Point| (p - state.pan) * iced::Transformation::scale(1.0 / state.zoom);
        let cursor_pos = cursor.position_in(bounds).unwrap_or_default();
        let cursor_pos = t(cursor_pos);

        let graph = self.graph.borrow();

        match event {
            Event::Mouse(iced::mouse::Event::ButtonPressed(Button::Left)) => {
                if cursor.position().is_some() {
                    // Port selection
                    for node_idx in graph.node_indices() {
                        let node = &graph[node_idx];
                        for (i, port) in node.output_ports.iter().enumerate() {
                            let port_pos = Point::new(
                                node.position.0 + node.size().width,
                                node.position.1 + &port.offset_px,
                            );
                            let dx = cursor_pos.x - port_pos.x;
                            let dy = cursor_pos.y - port_pos.y;
                            if dx * dx + dy * dy <= 36.0 {
                                state.action_ctx =
                                    ExclusiveActionCtx::ConnectingFromPort((node_idx, i));
                                state.last_cursor_position = Some(cursor_pos);
                                return (canvas::event::Status::Captured, None);
                            }
                        }
                        for (i, port) in node.input_ports.iter().enumerate() {
                            let port_pos =
                                Point::new(node.position.0, node.position.1 + &port.offset_px);
                            let dx = cursor_pos.x - port_pos.x;
                            let dy = cursor_pos.y - port_pos.y;
                            if dx * dx + dy * dy <= 36.0 {
                                if let ExclusiveActionCtx::ConnectingFromPort((src_idx, out_idx)) =
                                    state.action_ctx
                                {
                                    let from_port = graph[src_idx]
                                        .outputs
                                        .get_by_right(&out_idx)
                                        .unwrap()
                                        .clone();
                                    let to_port =
                                        graph[node_idx].inputs.get_by_right(&i).unwrap().clone();
                                    let msg = Some(Message::Canvas(CanvasMessage::AddEdge(
                                        src_idx,
                                        node_idx,
                                        EdgeData { from_port, to_port },
                                    )));
                                    state.action_ctx = ExclusiveActionCtx::None;
                                    return (canvas::event::Status::Captured, msg);
                                }
                            }
                        }
                    }

                    // Edge selection
                    // TODO: overhaul this
                    for edge in graph.edge_references() {
                        let from = &graph[edge.source()];
                        let to = &graph[edge.target()];
                        let from_pos = Point::new(
                            from.position.0 + from.size().width,
                            from.position.1 + from.size().height / 2.0,
                        );
                        let to_pos =
                            Point::new(to.position.0, to.position.1 + to.size().height / 2.0);
                        let mid_x = (from_pos.x + to_pos.x) / 2.0;
                        let mid_y = (from_pos.y + to_pos.y) / 2.0;
                        let dx = cursor_pos.x - mid_x;
                        let dy = cursor_pos.y - mid_y;
                        if dx * dx + dy * dy < 200.0 {
                            state.action_ctx = ExclusiveActionCtx::EdgeSelected(
                                edge.source(),
                                edge.target(),
                                edge.weight().clone(),
                            );
                            return (canvas::event::Status::Captured, None);
                        }
                    }

                    // Node selection
                    for node_idx in graph.node_indices().rev() {
                        let node = &graph[node_idx];
                        let node_rect = Rectangle {
                            x: node.position.0,
                            y: node.position.1,
                            width: node.size().width,
                            height: node.size().height,
                        };
                        if node_rect.contains(cursor_pos) {
                            state.action_ctx = ExclusiveActionCtx::NodeSelected(node_idx);
                            state.dragging_node = Some(node_idx);
                            state.last_cursor_position = Some(cursor_pos);
                            return (canvas::event::Status::Captured, None);
                        }
                    }
                }
            }
            Event::Mouse(iced::mouse::Event::ButtonReleased(Button::Left)) => {
                if state.dragging_node.is_some() {
                    state.dragging_node = None;
                    let msg = Some(Message::Canvas(CanvasMessage::Checkpoint));
                    return (canvas::event::Status::Captured, msg);
                }
            }
            Event::Mouse(iced::mouse::Event::ButtonPressed(Button::Middle)) => {
                state.panning = true;
            }
            Event::Mouse(iced::mouse::Event::ButtonReleased(Button::Middle)) => {
                state.panning = false;
            }
            Event::Mouse(iced::mouse::Event::CursorMoved { position: _ }) => {
                // Node drag
                let cursor_pos = cursor.position_in(bounds).unwrap_or_default();
                let cursor_pos = t(cursor_pos);

                if let Some(dragged) = state.dragging_node {
                    if let Some(last_pos) = state.last_cursor_position {
                        let delta = cursor_pos - last_pos;
                        let msg = Some(Message::Canvas(CanvasMessage::MoveNode(
                            dragged,
                            (delta.x, delta.y),
                        )));
                        state.last_cursor_position = Some(cursor_pos);
                        return (canvas::event::Status::Captured, msg);
                    }
                }

                // Pan
                if state.panning {
                    if let Some(last_pos) = state.last_cursor_position {
                        let delta = cursor_pos - last_pos;
                        state.pan = state.pan + delta;
                    }
                }
                state.last_cursor_position = Some(cursor_pos);
            }
            Event::Mouse(iced::mouse::Event::WheelScrolled { delta }) => {
                // Zoom
                let scroll_y = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. }
                    | iced::mouse::ScrollDelta::Pixels { y, .. } => y,
                };
                let factor = 1.1f32.powf(scroll_y);
                state.zoom *= factor;
            }
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: Key::Named(Named::Delete),
                ..
            }) => {
                // Delete
                if let ExclusiveActionCtx::EdgeSelected(src, dst, data) = &mut state.action_ctx {
                    let msg = Some(Message::Canvas(CanvasMessage::RemoveEdge(
                        *src,
                        *dst,
                        data.clone(),
                    )));

                    state.action_ctx = ExclusiveActionCtx::None;

                    return (canvas::event::Status::Captured, msg);
                } else if let ExclusiveActionCtx::NodeSelected(idx) = state.action_ctx {
                    state.action_ctx = ExclusiveActionCtx::None;
                    let msg = Some(Message::Canvas(CanvasMessage::RemoveNode(idx)));
                    return (canvas::event::Status::Captured, msg);
                }
            }
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                physical_key: Physical::Code(Code::KeyZ),
                modifiers: Modifiers::CTRL,
                ..
            }) => {
                // Undo
                state.action_ctx = ExclusiveActionCtx::None;
                state.dragging_node = None;
                let msg = Some(Message::Canvas(CanvasMessage::Undo));
                return (canvas::event::Status::Captured, msg);
            }
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                physical_key: Physical::Code(Code::KeyZ),
                modifiers,
                ..
            }) => {
                // Redo
                if modifiers == Modifiers::CTRL | Modifiers::SHIFT {
                    // Non-const pattern -> can't use in destructing pattern
                    state.action_ctx = ExclusiveActionCtx::None;
                    state.dragging_node = None;
                    let msg = Some(Message::Canvas(CanvasMessage::Redo));
                    return (canvas::event::Status::Captured, msg);
                }
            }
            Event::Mouse(iced::mouse::Event::ButtonPressed(Button::Right)) => {
                // Cancel
                state.action_ctx = ExclusiveActionCtx::None;
                return (canvas::event::Status::Captured, None);
            }
            _ => {}
        }

        (canvas::event::Status::Captured, None)
    }
}
