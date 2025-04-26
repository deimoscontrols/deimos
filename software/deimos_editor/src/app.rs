use std::{any::type_name_of_val, borrow::Cow, collections::HashMap, default, path::PathBuf};

use eframe::egui::{self, DragValue, TextStyle};
use egui_node_graph2::*;

use anyhow::Result;

use deimos::{
    calc::{Calc, PROTOTYPES},
    Controller,
};

// ========= First, define your user data types =============

/// The NodeData holds a custom data struct inside each node. It's useful to
/// store additional information that doesn't live in parameters. For this
/// example, the node data stores the template (i.e. the "type") of the node.
#[cfg_attr(feature = "persistence", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct MyNodeData {
    template: MyNodeTemplate,
}

impl MyNodeData {
    pub fn add_to_graph(&self, graph: &mut MyGraph, node_id: Option<NodeId>) {
        let input_scalar = |graph: &mut MyGraph, name: &str, node_id: NodeId| {
            graph.add_input_param(
                node_id,
                name.to_string(),
                MyDataType::Scalar,
                MyValueType::Scalar { value: 0.0 },
                InputParamKind::ConnectionOnly,
                false,
            );
        };

        let config_scalar = |graph: &mut MyGraph, name: &str, node_id: NodeId| {
            graph.add_input_param(
                node_id,
                name.to_string(),
                MyDataType::Scalar,
                MyValueType::Scalar { value: 0.0 },
                InputParamKind::ConstantOnly,
                true,
            );
        };

        let output_scalar = |graph: &mut MyGraph, name: &str, node_id: NodeId| {
            graph.add_output_param(node_id, name.to_string(), MyDataType::Scalar);
        };

        match &self.template {
            MyNodeTemplate::Calc { kind, name, calc } => {
                // Use the node id if it was provided; overwise, make a new one
                let node_id = node_id.unwrap_or_else(|| {
                    graph.add_node(kind.to_owned(), self.clone(), |graph, node_id| {
                        for config_name in calc.get_config().keys() {
                            config_scalar(graph, &config_name, node_id);
                        }

                        for input_name in calc.get_input_names() {
                            input_scalar(graph, &input_name, node_id);
                        }

                        for output_name in calc.get_output_names() {
                            output_scalar(graph, &output_name, node_id);
                        }
                    })
                });
            }
        }
    }
}

/// `DataType`s are what defines the possible range of connections when
/// attaching two ports together. The graph UI will make sure to not allow
/// attaching incompatible datatypes.
#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "persistence", derive(serde::Serialize, serde::Deserialize))]
pub enum MyDataType {
    Scalar,
}

/// In the graph, input parameters can optionally have a constant value. This
/// value can be directly edited in a widget inside the node itself.
///
/// There will usually be a correspondence between DataTypes and ValueTypes. But
/// this library makes no attempt to check this consistency. For instance, it is
/// up to the user code in this example to make sure no parameter is created
/// with a DataType of Scalar and a ValueType of Vec2.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(feature = "persistence", derive(serde::Serialize, serde::Deserialize))]
pub enum MyValueType {
    Scalar { value: f64 },
}

impl Default for MyValueType {
    fn default() -> Self {
        // NOTE: This is just a dummy `Default` implementation. The library
        // requires it to circumvent some internal borrow checker issues.
        Self::Scalar { value: 0.0 }
    }
}

/// NodeTemplate is a mechanism to define node templates. It's what the graph
/// will display in the "new node" popup. The user code needs to tell the
/// library how to convert a NodeTemplate into a Node.
#[derive(Clone)]
#[cfg_attr(feature = "persistence", derive(serde::Serialize, serde::Deserialize))]
pub enum MyNodeTemplate {
    Calc {
        kind: String,
        name: String,
        calc: Box<dyn Calc>,
    },
}

/// The response type is used to encode side-effects produced when drawing a
/// node in the graph. Most side-effects (creating new nodes, deleting existing
/// nodes, handling connections...) are already handled by the library, but this
/// mechanism allows creating additional side effects from user code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MyResponse {
    // SetActiveNode(NodeId),
    // ClearActiveNode,
}

/// The graph 'global' state. This state struct is passed around to the node and
/// parameter drawing callbacks. The contents of this struct are entirely up to
/// the user. For this example, we use it to keep track of the 'active' node.
#[derive(Default)]
#[cfg_attr(feature = "persistence", derive(serde::Serialize, serde::Deserialize))]
pub struct MyGraphState {
    pub active_node: Option<NodeId>,
}

// =========== Then, you need to implement some traits ============

// A trait for the data types, to tell the library how to display them
impl DataTypeTrait<MyGraphState> for MyDataType {
    fn data_type_color(&self, _user_state: &mut MyGraphState) -> egui::Color32 {
        match self {
            MyDataType::Scalar => egui::Color32::from_rgb(38, 109, 211),
        }
    }

    fn name(&self) -> Cow<'_, str> {
        match self {
            MyDataType::Scalar => Cow::Borrowed("scalar"),
        }
    }
}

// A trait for the node kinds, which tells the library how to build new nodes
// from the templates in the node finder
impl NodeTemplateTrait for MyNodeTemplate {
    type NodeData = MyNodeData;
    type DataType = MyDataType;
    type ValueType = MyValueType;
    type UserState = MyGraphState;
    type CategoryType = &'static str;

    fn node_finder_label(&self, _user_state: &mut Self::UserState) -> Cow<'_, str> {
        Cow::Borrowed(match self {
            MyNodeTemplate::Calc { kind: x, .. } => x,
            _ => panic!(),
        })
    }

    // this is what allows the library to show collapsible lists in the node finder.
    fn node_finder_categories(&self, _user_state: &mut Self::UserState) -> Vec<&'static str> {
        match self {
            MyNodeTemplate::Calc { .. } => vec!["Calc"],
        }
    }

    fn node_graph_label(&self, user_state: &mut Self::UserState) -> String {
        // It's okay to delegate this to node_finder_label if you don't want to
        // show different names in the node finder and the node itself.
        self.node_finder_label(user_state).into()
    }

    fn user_data(&self, _user_state: &mut Self::UserState) -> Self::NodeData {
        MyNodeData {
            template: self.clone(),
        }
    }

    /// Add a template default node to the graph
    fn build_node(
        &self,
        graph: &mut Graph<Self::NodeData, Self::DataType, Self::ValueType>,
        _user_state: &mut Self::UserState,
        node_id: NodeId,
    ) {
        let data = MyNodeData {
            template: self.clone(),
        };
        data.add_to_graph(graph, Some(node_id));
    }
}

pub struct AllMyNodeTemplates;
impl NodeTemplateIter for AllMyNodeTemplates {
    type Item = MyNodeTemplate;

    fn all_kinds(&self) -> Vec<Self::Item> {
        // List of node kinds for dropdown menu
        let mut kinds = vec![];
        for (k, v) in PROTOTYPES.iter() {
            kinds.push(MyNodeTemplate::Calc {
                kind: k.clone(),
                name: "".into(),
                calc: v.clone(),
            });
        }

        kinds
    }
}

impl WidgetValueTrait for MyValueType {
    type Response = MyResponse;
    type UserState = MyGraphState;
    type NodeData = MyNodeData;
    fn value_widget(
        &mut self,
        param_name: &str,
        _node_id: NodeId,
        ui: &mut egui::Ui,
        _user_state: &mut MyGraphState,
        _node_data: &MyNodeData,
    ) -> Vec<MyResponse> {
        // This trait is used to tell the library which UI to display for the
        // inline parameter widgets.
        match self {
            MyValueType::Scalar { value } => {
                ui.horizontal(|ui| {
                    ui.label(param_name);
                    ui.add(DragValue::new(value).custom_formatter(|x, _| {
                        if x.abs() >= 1e4 || x.abs() < 1e-3 {
                            format!("{x:.6e}")
                        } else {
                            format!("{x:.6}")
                        }
                    }))
                });
            }
        }
        // This allows you to return your responses from the inline widgets.
        Vec::new()
    }
}

impl UserResponseTrait for MyResponse {}
impl NodeDataTrait for MyNodeData {
    type Response = MyResponse;
    type UserState = MyGraphState;
    type DataType = MyDataType;
    type ValueType = MyValueType;

    // This method will be called when drawing each node. This allows adding
    // extra ui elements inside the nodes. In this case, we create an "active"
    // button which introduces the concept of having an active node in the
    // graph. This is done entirely from user code with no modifications to the
    // node graph library.
    fn bottom_ui(
        &self,
        ui: &mut egui::Ui,
        node_id: NodeId,
        _graph: &Graph<MyNodeData, MyDataType, MyValueType>,
        user_state: &mut Self::UserState,
    ) -> Vec<NodeResponse<MyResponse, MyNodeData>>
    where
        MyResponse: UserResponseTrait,
    {
        let responses = vec![];
        responses
    }
}

type MyGraph = Graph<MyNodeData, MyDataType, MyValueType>;
type MyEditorState =
    GraphEditorState<MyNodeData, MyDataType, MyValueType, MyNodeTemplate, MyGraphState>;

#[derive(Default)]
pub struct Editor {
    // The `GraphEditorState` is the top-level object. You "register" all your
    // custom types by specifying it as its generic parameters.
    state: MyEditorState,

    user_state: MyGraphState,
}

#[cfg(feature = "persistence")]
const PERSISTENCE_KEY: &str = "deimos_editor";

#[cfg(feature = "persistence")]
impl Editor {
    /// If the persistence feature is enabled, Called once before the first frame.
    /// Load previous app state (if any).
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let state = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, PERSISTENCE_KEY))
            .unwrap_or_default();
        Self {
            state,
            user_state: MyGraphState::default(),
        }
    }

    pub fn load_file(&mut self, path: PathBuf) -> Result<()> {
        use std::any::Any;

        // Add calcs
        let file_contents = std::fs::read_to_string(path)?;
        let controller: Controller = serde_json::from_str(&file_contents)?;

        for (name, calc) in controller.calcs() {
            // let kind = type_name_of_val({calc as &dyn Any}.downcast_ref().unwrap());
            let kind = calc.kind();
            let node = MyNodeData {
                template: MyNodeTemplate::Calc {
                    kind,
                    name: name.to_owned(),
                    calc: calc.clone(),
                },
            };

            node.add_to_graph(&mut self.state.graph, None);
        }

        Ok(())
    }
}

impl eframe::App for Editor {
    #[cfg(feature = "persistence")]
    /// If the persistence function is enabled,
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PERSISTENCE_KEY, &self.state);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    /// Put your widgets into a `SidePanel`, `TopPanel`, `CentralPanel`, `Window` or `Area`.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // TOP PANEL
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            // MENU BAR
            egui::menu::bar(ui, |ui| {
                // Light/dark mode selector
                egui::widgets::global_theme_preference_switch(ui);

                // File menu
                if ui
                    .button("Load")
                    .on_hover_text("Load a json file representing a deimos Controller")
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_directory(std::env::current_dir().unwrap_or("./".into()))
                        .add_filter("json", &["json"])
                        .pick_file()
                    {
                        // self.picked_path = Some(path.display().to_string());
                        self.load_file(path).unwrap();
                    }
                }
            });
        });

        // CENTER PANEL
        let _graph_response = egui::CentralPanel::default()
            .show(ctx, |ui| {
                // NODE EDITOR
                self.state.draw_graph_editor(
                    ui,
                    AllMyNodeTemplates,
                    &mut self.user_state,
                    Vec::default(),
                )
            })
            .inner;

        // Node selection callback
        if let Some(node) = self.user_state.active_node {
            if self.state.graph.nodes.contains_key(node) {
                let text = "";
                ctx.debug_painter().text(
                    egui::pos2(10.0, 35.0),
                    egui::Align2::LEFT_TOP,
                    text,
                    TextStyle::Button.resolve(&ctx.style()),
                    egui::Color32::WHITE,
                );
            } else {
                self.user_state.active_node = None;
            }
        }
    }
}
