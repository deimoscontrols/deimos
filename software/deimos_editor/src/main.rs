use canvas::Event;
use iced::keyboard::Key;
use iced::keyboard::key::Named;
use iced::mouse::Cursor;
use iced::{
    Application, Command, Element, Length, Point, Rectangle, Renderer, Settings, Theme,
    widget::{
        Column,
        canvas::{self, Canvas, Frame, Geometry, Path, Program, Text},
    },
};
use petgraph::graph::{Graph, NodeIndex};
use petgraph::visit::EdgeRef;

#[derive(Debug)]
struct NodeData {
    name: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    position: Point,
}

#[derive(Debug, PartialEq)]
struct EdgeData {
    from_port: String,
    to_port: String,
}

pub fn main() -> iced::Result {
    NodeEditor::run(Settings::default())
}

struct NodeEditor {
    editor_state: EditorState,
}

#[derive(Default)]
struct EditorState {
    dragged_node: Option<NodeIndex>,
    last_cursor_position: Option<Point>,
    geometry: Option<Geometry>,
    connecting_from: Option<(NodeIndex, usize)>,
    selected_edge: Option<(NodeIndex, NodeIndex)>,
    graph: Graph<NodeData, EdgeData>,
}

#[derive(Debug, Clone, Copy)]
enum Message {}

impl Application for NodeEditor {
    type Executor = iced::executor::Default;
    type Message = Message;
    type Theme = Theme;
    type Flags = ();

    fn new(_: ()) -> (Self, Command<Message>) {
        let graph = Graph::<NodeData, EdgeData>::new();
        (
            Self {
                editor_state: EditorState {
                    dragged_node: None,
                    last_cursor_position: None,
                    geometry: None,
                    connecting_from: None,
                    selected_edge: None,
                    graph,
                },
            },
            Command::none(),
        )
    }

    fn title(&self) -> String {
        String::from("Node Editor Example")
    }

    fn update(&mut self, _message: Message) -> Command<Message> {
        Command::none()
    }

    fn view(&self) -> Element<Message> {
        let canvas = Canvas::new(EditorCanvas {
            editor_state: &self.editor_state,
        })
        .width(Length::Fill)
        .height(Length::Fill);

        Column::new().push(canvas).into()
    }
}

struct EditorCanvas<'a> {
    editor_state: &'a EditorState,
}

impl<'a> Program<Message> for EditorCanvas<'a> {
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

        frame.fill_rectangle(
            Point::ORIGIN,
            frame.size(),
            iced::Color::from_rgb(0.1, 0.1, 0.1),
        );

        for node_idx in state.graph.node_indices() {
            let node = &state.graph[node_idx];
            let rect = Path::rectangle(node.position, iced::Size::new(100.0, 60.0));
            frame.fill(&rect, iced::Color::from_rgb(0.3, 0.3, 0.5));
            frame.stroke(
                &rect,
                canvas::Stroke::default().with_color(iced::Color::WHITE),
            );

            frame.fill_text(Text {
                content: node.name.clone(),
                position: Point::new(node.position.x + 6.0, node.position.y + 8.0),
                color: iced::Color::WHITE,
                size: 16.0.into(),
                ..Default::default()
            });

            for (i, port) in node.outputs.iter().enumerate() {
                let port_pos = Point::new(
                    node.position.x + 100.0,
                    node.position.y + 20.0 + i as f32 * 15.0,
                );
                let port_circle = Path::circle(port_pos, 4.0);
                frame.fill(&port_circle, iced::Color::WHITE);
                frame.fill_text(Text {
                    content: port.clone(),
                    position: Point::new(port_pos.x + 6.0, port_pos.y + 4.0),
                    color: iced::Color::WHITE,
                    size: 12.0.into(),
                    ..Default::default()
                });
            }

            for (i, port) in node.inputs.iter().enumerate() {
                let port_pos =
                    Point::new(node.position.x, node.position.y + 20.0 + i as f32 * 15.0);
                let port_circle = Path::circle(port_pos, 4.0);
                frame.fill(&port_circle, iced::Color::WHITE);
                frame.fill_text(Text {
                    content: port.clone(),
                    position: Point::new(
                        port_pos.x - (port.len() as f32 * 6.0) - 8.0,
                        port_pos.y + 4.0,
                    ),
                    color: iced::Color::WHITE,
                    size: 12.0.into(),
                    ..Default::default()
                });
            }
        }

        for edge in state.graph.edge_references() {
            let from = &state.graph[edge.source()];
            let to = &state.graph[edge.target()];
            let from_pos = Point::new(from.position.x + 100.0, from.position.y + 30.0);
            let to_pos = Point::new(to.position.x, to.position.y + 30.0);
            let ctrl1 = Point::new(from_pos.x + 50.0, from_pos.y);
            let ctrl2 = Point::new(to_pos.x - 50.0, to_pos.y);

            let path = Path::new(|builder| {
                builder.move_to(from_pos);
                builder.bezier_curve_to(ctrl1, ctrl2, to_pos);
            });

            let is_selected = Some((edge.source(), edge.target())) == state.selected_edge;
            let color = if is_selected {
                iced::Color::from_rgb(1.0, 0.2, 0.2)
            } else {
                iced::Color::WHITE
            };

            frame.stroke(
                &path,
                canvas::Stroke::default().with_color(color).with_width(2.0),
            );
        }

        if let (Some((from_idx, port_idx)), Some(cursor_pos)) =
            (state.connecting_from, state.last_cursor_position)
        {
            let node = &state.graph[from_idx];
            let start = Point::new(
                node.position.x + 100.0,
                node.position.y + 20.0 + port_idx as f32 * 15.0,
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
        // Example data
        if state.graph.node_indices().len() == 0 {
            let a = state.graph.add_node(NodeData {
                name: "Add".into(),
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["sum".into()],
                position: Point::new(100.0, 100.0),
            });

            let b = state.graph.add_node(NodeData {
                name: "Display".into(),
                inputs: vec!["input".into()],
                outputs: vec![],
                position: Point::new(400.0, 200.0),
            });

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
            Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                if let Some(cursor_pos) = cursor.position() {
                    for node_idx in state.graph.node_indices() {
                        let node = &state.graph[node_idx];
                        for (i, _port) in node.outputs.iter().enumerate() {
                            let port_pos = Point::new(
                                node.position.x + 100.0,
                                node.position.y + 20.0 + i as f32 * 15.0,
                            );
                            let dx = cursor_pos.x - port_pos.x;
                            let dy = cursor_pos.y - port_pos.y;
                            if dx * dx + dy * dy <= 36.0 {
                                state.connecting_from = Some((node_idx, i));
                                state.last_cursor_position = Some(cursor_pos);
                                return (canvas::event::Status::Captured, None);
                            }
                        }
                        for (i, _port) in node.inputs.iter().enumerate() {
                            let port_pos = Point::new(
                                node.position.x,
                                node.position.y + 20.0 + i as f32 * 15.0,
                            );
                            let dx = cursor_pos.x - port_pos.x;
                            let dy = cursor_pos.y - port_pos.y;
                            if dx * dx + dy * dy <= 36.0 {
                                if let Some((src_idx, out_idx)) = state.connecting_from.take() {
                                    let from_port = state.graph[src_idx].outputs[out_idx].clone();
                                    let to_port = state.graph[node_idx].inputs[i].clone();
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

                    for edge in state.graph.edge_references() {
                        let from = &state.graph[edge.source()];
                        let to = &state.graph[edge.target()];
                        let from_pos = Point::new(from.position.x + 100.0, from.position.y + 30.0);
                        let to_pos = Point::new(to.position.x, to.position.y + 30.0);
                        let mid_x = (from_pos.x + to_pos.x) / 2.0;
                        let mid_y = (from_pos.y + to_pos.y) / 2.0;
                        let dx = cursor_pos.x - mid_x;
                        let dy = cursor_pos.y - mid_y;
                        if dx * dx + dy * dy < 100.0 {
                            state.selected_edge = Some((edge.source(), edge.target()));
                            return (canvas::event::Status::Captured, None);
                        }
                    }
                }
            }
            Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                state.last_cursor_position = Some(position);
            }
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: Key::Named(Named::Delete),
                ..
            }) => {
                if let Some((src, dst)) = state.selected_edge.take() {
                    if let Some(edge_idx) = state.graph.find_edge(src, dst) {
                        state.graph.remove_edge(edge_idx);
                    }
                }
            }
            _ => {}
        }

        (canvas::event::Status::Captured, None)
    }
}
