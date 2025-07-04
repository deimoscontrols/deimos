//! A control program with several DAQ modules connected via UDP and time-synchronized.
//!
//! Demonstrated here:
//!   * Setting up a simple control program and connecting to hardware
//!   * Storing data
//!   * Performing calculations in the loop
//!   * Serialization and deserialization of the control program

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::calc::{Constant, Sin};
use crate::peripheral::{AnalogIRev3, AnalogIRev4};
use controller::context::ControllerCtx;
use deimos::calc::SequenceMachine;
use deimos::calc::sequence_machine::{MachineCfg, ThreshOp, Timeout, Transition};
use deimos::*;

fn main() {
    // Set op name
    // None -> Let the controller set the name of the op automatically
    let op_dir: PathBuf = "./software/deimos/examples".into();
    let op_name_path = op_dir.join("op_name.tmp");
    let op_name = std::fs::read_to_string(&op_name_path).unwrap_or_else(|_| {
        format!(
            "{:?}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
        )
    });
    std::fs::write(op_name_path, &op_name).unwrap();

    // Collect initalizers for custom peripherals, if needed
    let peripheral_plugins = None;

    // Set control rate
    let rate_hz = 100.0;
    let dt_ns = (1e9_f64 / rate_hz).ceil() as u32;

    // Define idle controller
    let mut ctx = ControllerCtx::default();
    ctx.op_name = op_name;
    ctx.dt_ns = dt_ns;
    ctx.op_dir = op_dir.clone();
    //   For a demo control network, assume that the control server does not have
    //   a static address and may drop out for a few seconds while renewing its IP address.
    //   For a real network, the control server and peripherals should be assigned
    //   static addresses, and this limit can be comfortably set as low as 2-3 cycles.
    ctx.controller_loss_of_contact_limit = (4.0 * rate_hz).min(1e4) as u16;
    ctx.peripheral_loss_of_contact_limit = (4.0 * rate_hz).min(1e4) as u16;
    let mut controller = Controller::new(ctx);

    // Scan for peripherals on LAN
    let scanned_peripherals = controller.scan(10, &peripheral_plugins);
    println!("Scan found: {scanned_peripherals:?}\n");

    // Associate peripherals
    controller.add_peripheral("p1", Box::new(AnalogIRev3 { serial_number: 1 }));
    controller.add_peripheral("p2", Box::new(AnalogIRev3 { serial_number: 2 }));
    controller.add_peripheral("p3", Box::new(AnalogIRev4 { serial_number: 1 }));
    controller.add_peripheral("p4", Box::new(AnalogIRev4 { serial_number: 2 }));
    controller.add_peripheral("p5", Box::new(AnalogIRev4 { serial_number: 3 }));
    controller.add_peripheral("p6", Box::new(AnalogIRev4 { serial_number: 4 }));
    // controller.add_peripheral("p7", Box::new(AnalogIRev4 { serial_number: 5 }));
    controller.add_peripheral("p8", Box::new(AnalogIRev4 { serial_number: 6 }));

    // Set up database dispatchers
    let timescale_dispatcher: Box<dyn Dispatcher> = Box::new(TimescaleDbDispatcher::new(
        "tsdb",
        "/run/postgresql/", // Unix socket interface; TCP works as well
        "jlogan",
        "POSTGRES_PW",
        Duration::from_nanos(1),
        1,
    ));
    let csv_dispatcher: Box<dyn Dispatcher> =
        Box::new(CsvDispatcher::new(50, dispatcher::Overflow::Wrap));
    controller.add_dispatcher(timescale_dispatcher);
    controller.add_dispatcher(csv_dispatcher);

    // Set up calc graph
    let duty = Constant::new(0.5, true);
    let freq = Sin::new(1.0 / (rate_hz / 100.0), 0.25, 100.0, 250_000.0, true);
    let freq1 = Sin::new(20.0, 0.25, 10.0, 200.0, true);
    controller.add_calc("duty", Box::new(duty));
    controller.add_calc("freq", Box::new(freq));
    controller.add_calc("freq1", Box::new(freq1));
    controller.set_peripheral_input_source("p1.pwm0_duty", "duty.y");
    controller.set_peripheral_input_source("p1.pwm0_freq", "freq.y");
    controller.set_peripheral_input_source("p1.pwm1_duty", "duty.y");
    controller.set_peripheral_input_source("p1.pwm1_freq", "freq1.y");
    controller.set_peripheral_input_source("p1.pwm3_duty", "sequence_machine.duty");
    controller.set_peripheral_input_source("p1.pwm3_freq", "freq.y");

    let timeouts = BTreeMap::from([
        ("low".to_owned(), Timeout::Loop),
        ("high".to_owned(), Timeout::Transition("low".to_owned())),
    ]);

    let transitions: BTreeMap<String, BTreeMap<String, Vec<Transition>>> = BTreeMap::from([
        (
            "low".to_owned(),
            BTreeMap::from([(
                "high".to_owned(),
                vec![Transition::ConstantThresh(
                    "freq.y".to_owned(),
                    ThreshOp::Gt { by: 0.0 },
                    100_000.0,
                )],
            )]),
        ),
        ("high".to_owned(), BTreeMap::from([])),
    ]);

    let cfg = MachineCfg {
        save_outputs: true,
        entry: "low".to_owned(),
        link_folder: Some("machine".to_owned()),
        timeouts,
        transitions,
    };
    let cfg_str = serde_json::to_string_pretty(&cfg).unwrap();

    let machine_dir = op_dir.join("machine");
    let fp = machine_dir.join("cfg.json");
    std::fs::write(fp, cfg_str).unwrap();

    let machine = SequenceMachine::load_folder(&machine_dir).unwrap();
    controller.add_calc("sequence_machine", Box::new(machine));

    // Serialize and deserialize the controller (for demonstration purposes)
    let serialized_controller = serde_json::to_string_pretty(&controller).unwrap();
    let _: Controller = serde_json::from_str(&serialized_controller).unwrap();
    // std::fs::write("./multi_daq.json", &serialized_controller);

    // Run the control program
    println!("Starting controller");
    controller.run(&peripheral_plugins).unwrap();
}
