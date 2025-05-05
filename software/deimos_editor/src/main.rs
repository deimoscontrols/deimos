use std::collections::VecDeque;

use canvas::Event;
use iced::Font;
use iced::border::Radius;
use iced::keyboard::key::{Code, Named, Physical};
use iced::keyboard::{Key, Modifiers};
use iced::mouse::{Button, Cursor};
use iced::{
    Element, Length, Point, Rectangle, Renderer, Theme, Vector,
    widget::{
        Column,
        canvas::{self, Canvas, Frame, Geometry, Path, Program, Text},
    },
};
use petgraph::graph::{Graph, NodeIndex};
use petgraph::visit::EdgeRef;

use serde::{Deserialize, Serialize};
use serde_json;

use bimap::BiBTreeMap;

pub fn main() -> iced::Result {
    iced::application("Deimos Editor", NodeEditor::update, NodeEditor::view)
        .theme(|_| Theme::Dark)
        .centered()
        .antialiasing(true)
        .run()
}

#[derive(Debug, Serialize, Deserialize)]
struct Port {
    /// Relative to top of node frame
    offset_px: f32,
}

#[derive(Debug, Serialize, Deserialize)]
struct NodeData {
    name: String,
    inputs: BiBTreeMap<String, usize>,
    outputs: BiBTreeMap<String, usize>,
    position: (f32, f32),
    input_ports: Vec<Port>,
    output_ports: Vec<Port>,
    size: (f32, f32),
}

impl NodeData {
    pub fn new(
        name: String,
        inputs: Vec<String>,
        outputs: Vec<String>,
        position: (f32, f32),
    ) -> NodeData {
        let mut input_map = BiBTreeMap::new();
        let mut output_map = BiBTreeMap::new();

        let mut input_ports = Vec::with_capacity(inputs.len());
        let mut output_ports = Vec::with_capacity(outputs.len());

        // Determine y-position of each port within the node widget
        let mut inp_offs = 0.0_f32;
        inputs.iter().enumerate().for_each(|(i, n)| {
            inp_offs += 20.0;
            input_map.insert(n.clone(), i);
            input_ports.push(Port {
                offset_px: inp_offs.into(),
            })
        });

        let mut out_offs = 0.0_f32;
        outputs.iter().enumerate().for_each(|(i, n)| {
            out_offs += 20.0;
            output_map.insert(n.clone(), i);
            output_ports.push(Port {
                offset_px: out_offs.into(),
            })
        });

        // Determine width and height of node widget
        let width = 100.0;
        let height = inp_offs.max(out_offs) + 20.0;
        let size = (width, height);

        Self {
            name,
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
        let ind = self.inputs.get_by_left(name).unwrap();
        &self.input_ports[*ind]
    }

    pub fn get_output_port(&self, name: &str) -> &Port {
        let ind = self.outputs.get_by_left(name).unwrap();
        &self.output_ports[*ind]
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct EdgeData {
    from_port: String,
    to_port: String,
}

#[derive(Debug, Clone, Copy)]
enum Message {}

#[derive(Default)]
struct NodeEditor {}

impl NodeEditor {
    fn update(_state: &mut Self, _message: Message) {}

    fn view(_state: &Self) -> Element<Message> {
        let canvas = Canvas::new(EditorCanvas {
            _v: core::marker::PhantomData,
        })
        .width(Length::Fill)
        .height(Length::Fill);

        Column::new().push(canvas).into()
    }
}

/// Ongoing actions that are mutually exclusive
#[derive(Default, PartialEq, Eq)]
enum ExclusiveActionCtx {
    #[default]
    None,
    NodeSelected(NodeIndex),
    EdgeSelected((NodeIndex, NodeIndex)),
    ConnectingFromPort((NodeIndex, usize)),
}

#[derive(Default)]
struct EditorState {
    pan: Vector,
    zoom: f32,
    panning: bool,
    dragging_node: Option<NodeIndex>,
    last_cursor_position: Option<Point>,
    action_ctx: ExclusiveActionCtx,
    graph: Graph<NodeData, EdgeData>,
    edit_log: VecDeque<String>,
    redo_log: Vec<String>,
}

impl EditorState {
    fn checkpoint(&mut self) {
        // Save checkpoint
        let entry = serde_json::to_string(&self.graph).unwrap();
        self.edit_log.push_back(entry);
        if self.edit_log.len() > 30 {
            let _ = self.edit_log.pop_front();
        }

        // Clear redo, which will no longer be valid after other edits
        self.redo_log.clear();
    }

    fn undo(&mut self) {
        // Move the latest state to the redo log
        if let Some(latest) = self.edit_log.pop_back() {
            self.redo_log.push(latest);
        }

        // Reset to the previous state, or if there is no previous state,
        // clear the graph.
        if let Some(previous) = self.edit_log.front() {
            self.graph = serde_json::from_str(previous).unwrap();
        } else {
            self.graph.clear();
        }

        // Clear selections and ongoing actions, which may no longer be valid
        self.action_ctx = ExclusiveActionCtx::None;
        self.dragging_node = None;
    }

    fn redo(&mut self) {
        // Reset to next state and move that checkpoint back to the edit log
        if let Some(next) = self.redo_log.pop() {
            self.graph = serde_json::from_str(&next).unwrap();
            self.edit_log.push_front(next);
        }

        // Clear selections and ongoing actions, which may no longer be valid
        self.action_ctx = ExclusiveActionCtx::None;
        self.dragging_node = None;
    }
}

struct EditorCanvas<'a> {
    _v: core::marker::PhantomData<&'a usize>,
}

impl<'a> Program<Message> for EditorCanvas<'a> {
    type State = EditorState;

    fn draw(
        &self,
        state: &EditorState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.translate(state.pan);
        frame.scale(state.zoom);

        // Background
        frame.fill_rectangle(
            Point::ORIGIN,
            frame.size(),
            iced::Color::from_rgb(0.1, 0.1, 0.1),
        );

        // Draw edges
        for edge in state.graph.edge_references() {
            let from = &state.graph[edge.source()];
            let to = &state.graph[edge.target()];
            let from_port_name = &edge.weight().from_port;
            let to_port_name = &edge.weight().to_port;
            let from_port = from.get_output_port(from_port_name);
            let to_port = to.get_input_port(to_port_name);

            let from_pos = Point::new(
                from.position.0 + from.size().width,
                from.position.1 + from_port.offset_px,
            );
            let to_pos = Point::new(to.position.0, to.position.1 + to_port.offset_px);
            let ctrl1 = Point::new(from_pos.x + 50.0, from_pos.y);
            let ctrl2 = Point::new(to_pos.x - 50.0, to_pos.y);

            let path = Path::new(|builder| {
                builder.move_to(from_pos);
                builder.bezier_curve_to(ctrl1, ctrl2, to_pos);
            });

            let is_selected = ExclusiveActionCtx::EdgeSelected((edge.source(), edge.target()))
                == state.action_ctx;
            let color = if is_selected {
                iced::Color::from_rgb(1.0, 0.2, 0.2)
            } else {
                iced::Color::from_rgb(0.6, 0.6, 0.6)
            };

            frame.stroke(
                &path,
                canvas::Stroke::default().with_color(color).with_width(2.0),
            );
        }

        // Draw nodes
        for node_idx in state.graph.node_indices() {
            let node = &state.graph[node_idx];

            // Border
            let rect = Path::rounded_rectangle(node.position(), node.size(), Radius::new(5.0));
            let color = if ExclusiveActionCtx::NodeSelected(node_idx) == state.action_ctx {
                iced::Color::from_rgb(1.0, 0.2, 0.2) // Highlight selected
            } else {
                iced::Color::WHITE
            };
            frame.fill(&rect, iced::Color::from_rgb(0.3, 0.3, 0.5));
            frame.stroke(&rect, canvas::Stroke::default().with_color(color));

            // Label
            frame.fill_text(Text {
                content: node.name.clone(),
                position: Point::new(node.position.0 + 6.0, node.position.1 + 8.0),
                color,
                size: 16.0.into(),
                font: Font::MONOSPACE,
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
                frame.fill(&port_circle, iced::Color::WHITE);
                frame.fill_text(Text {
                    content: port_name.clone(),
                    position: Point::new(port_pos.x + 6.0, port_pos.y - 8.0),
                    color: iced::Color::WHITE,
                    size: 12.0.into(),
                    font: Font::MONOSPACE,
                    ..Default::default()
                });
            }

            // Input ports
            for (i, port_name) in node.inputs.left_values().enumerate() {
                let port = &node.input_ports[i];
                let port_pos = Point::new(node.position.0, node.position.1 + port.offset_px);
                let port_circle = Path::circle(port_pos, 4.0);
                frame.fill(&port_circle, iced::Color::WHITE);
                frame.fill_text(Text {
                    content: port_name.clone(),
                    position: Point::new(
                        port_pos.x - (port_name.len() as f32 * 6.0) - 8.0,
                        port_pos.y - 8.0,
                    ),
                    color: iced::Color::WHITE,
                    size: 12.0.into(),
                    font: Font::MONOSPACE,
                    ..Default::default()
                });
            }
        }

        // Draw in-progress connection
        if let (ExclusiveActionCtx::ConnectingFromPort((from_idx, port_idx)), Some(cursor_pos)) =
            (&state.action_ctx, state.last_cursor_position)
        {
            let node = &state.graph[*from_idx];
            let start = Point::new(
                node.position.0 + node.size().width,
                node.position.1 + 20.0 + *port_idx as f32 * 15.0,
            );
            let ctrl1 = Point::new(start.x + 50.0, start.y);
            let ctrl2 = Point::new(cursor_pos.x - 50.0, cursor_pos.y);

            let preview_path = Path::new(|builder| {
                builder.move_to(start);
                builder.bezier_curve_to(ctrl1, ctrl2, cursor_pos);
            });

            frame.stroke(
                &preview_path,
                canvas::Stroke::default()
                    .with_color(iced::Color::from_rgb(0.8, 0.8, 0.2))
                    .with_width(2.0),
            );
        }

        vec![frame.into_geometry()]
    }

    fn update(
        &self,
        state: &mut EditorState,
        event: Event,
        _bounds: Rectangle,
        cursor: Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        // Make sure we don't divide by zero later when we calculate the zoom scaling
        if state.zoom == 0.0 {
            state.zoom = 1.0;
        }

        // Example data
        if state.graph.node_indices().len() == 0 {
            let a = state.graph.add_node(NodeData::new(
                "Add".into(),
                vec!["a".into(), "b".into()],
                vec!["sum".into()],
                (100.0, 100.0),
            ));

            let b = state.graph.add_node(NodeData::new(
                "Display".into(),
                vec!["input".into()],
                vec![],
                (400.0, 200.0),
            ));

            state.graph.add_edge(
                a,
                b,
                EdgeData {
                    from_port: "sum".into(),
                    to_port: "input".into(),
                },
            );
        }

        match event {
            Event::Mouse(iced::mouse::Event::ButtonPressed(Button::Left)) => {
                if let Some(cursor_pos) = cursor.position() {
                    let pos =
                        (cursor_pos - state.pan) * iced::Transformation::scale(1.0 / state.zoom);

                    // Port selection
                    for node_idx in state.graph.node_indices() {
                        let node = &state.graph[node_idx];
                        for (i, _port) in node.outputs.iter().enumerate() {
                            let port_pos = Point::new(
                                node.position.0 + 100.0,
                                node.position.1 + 20.0 + i as f32 * 15.0,
                            );
                            let dx = pos.x - port_pos.x;
                            let dy = pos.y - port_pos.y;
                            if dx * dx + dy * dy <= 36.0 {
                                state.action_ctx =
                                    ExclusiveActionCtx::ConnectingFromPort((node_idx, i));
                                state.last_cursor_position = Some(cursor_pos);
                                return (canvas::event::Status::Captured, None);
                            }
                        }
                        for (i, _port) in node.inputs.iter().enumerate() {
                            let port_pos = Point::new(
                                node.position.0,
                                node.position.1 + 20.0 + i as f32 * 15.0,
                            );
                            let dx = pos.x - port_pos.x;
                            let dy = pos.y - port_pos.y;
                            if dx * dx + dy * dy <= 36.0 {
                                if let ExclusiveActionCtx::ConnectingFromPort((src_idx, out_idx)) =
                                    state.action_ctx
                                {
                                    let from_port = state.graph[src_idx]
                                        .outputs
                                        .get_by_right(&out_idx)
                                        .unwrap()
                                        .clone();
                                    let to_port = state.graph[node_idx]
                                        .inputs
                                        .get_by_right(&i)
                                        .unwrap()
                                        .clone();
                                    state.graph.add_edge(
                                        src_idx,
                                        node_idx,
                                        EdgeData { from_port, to_port },
                                    );
                                    return (canvas::event::Status::Captured, None);
                                }
                            }
                        }
                    }

                    // Edge selection
                    for edge in state.graph.edge_references() {
                        let from = &state.graph[edge.source()];
                        let to = &state.graph[edge.target()];
                        let from_pos = Point::new(from.position.0 + 100.0, from.position.1 + 30.0);
                        let to_pos = Point::new(to.position.0, to.position.1 + 30.0);
                        let mid_x = (from_pos.x + to_pos.x) / 2.0;
                        let mid_y = (from_pos.y + to_pos.y) / 2.0;
                        let dx = pos.x - mid_x;
                        let dy = pos.y - mid_y;
                        if dx * dx + dy * dy < 100.0 {
                            state.action_ctx =
                                ExclusiveActionCtx::EdgeSelected((edge.source(), edge.target()));
                            return (canvas::event::Status::Captured, None);
                        }
                    }

                    // Node selection
                    for node_idx in state.graph.node_indices().rev() {
                        let node = &state.graph[node_idx];
                        let node_rect = Rectangle {
                            x: node.position.0,
                            y: node.position.1,
                            width: 100.0,
                            height: 60.0,
                        };
                        if node_rect.contains(pos) {
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
                    state.checkpoint();
                }
            }
            Event::Mouse(iced::mouse::Event::ButtonPressed(Button::Middle)) => {
                state.panning = true;
            }
            Event::Mouse(iced::mouse::Event::ButtonReleased(Button::Middle)) => {
                state.panning = false;
            }
            Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                // Node drag
                if let Some(dragged) = state.dragging_node {
                    if let Some(last_pos) = state.last_cursor_position {
                        let delta =
                            (position - last_pos) * iced::Transformation::scale(1.0 / state.zoom);
                        if let Some(node) = state.graph.node_weight_mut(dragged) {
                            node.position.0 += delta.x;
                            node.position.1 += delta.y;
                        }
                    }
                }

                // Pan
                if state.panning {
                    if let Some(prev) = state.last_cursor_position {
                        let dx = position.x - prev.x;
                        let dy = position.y - prev.y;
                        state.pan = state.pan + Vector::new(dx, dy);
                    }
                }
                state.last_cursor_position = Some(position);
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
                if let ExclusiveActionCtx::EdgeSelected((src, dst)) = state.action_ctx {
                    if let Some(edge_idx) = state.graph.find_edge(src, dst) {
                        state.graph.remove_edge(edge_idx);
                        state.action_ctx = ExclusiveActionCtx::None;
                        state.checkpoint();
                    }
                } else if let ExclusiveActionCtx::NodeSelected(idx) = state.action_ctx {
                    state.graph.remove_node(idx);
                    state.action_ctx = ExclusiveActionCtx::None;
                    state.checkpoint();
                }
            }
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                physical_key: Physical::Code(Code::KeyZ),
                modifiers: Modifiers::CTRL,
                ..
            }) => {
                // Undo
                state.undo();
            }
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                physical_key: Physical::Code(Code::KeyZ),
                modifiers,
                ..
            }) => {
                // Redo
                if modifiers == Modifiers::CTRL | Modifiers::SHIFT {
                    // Non-const pattern -> can't use in destructing pattern
                    state.redo();
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
