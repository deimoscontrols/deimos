"""A 1kHz control program with a single DAQ.

Demonstrated here:
  * Setting up a simple control program and connecting to hardware
  * Storing data
  * Performing calculations in the loop
  * Serialization and deserialization of the control program
"""

from pathlib import Path

import deimos
from deimos import Overflow, peripheral


def main() -> None:
    # Define idle controller
    here = Path(__file__).parent.resolve()
    controller = deimos.Controller("basic_example", str(here), 1000.0)
    controller.dt_ns = int((1e9 / 1000.0) + 0.999999999)  # 1000 Hz (ceil)

    # Associate hardware peripherals
    controller.add_peripheral("p1", peripheral.AnalogIRev4(1))

    # Set up data targets
    csv_dispatcher = deimos.dispatcher.CsvDispatcher(50, Overflow.wrap()) 
    controller.add_dispatcher("csv", csv_dispatcher)

    # Serialize and deserialize the controller (for demonstration purposes)
    serialized_controller = controller.to_json()
    _ = deimos.Controller.from_json(serialized_controller)

    # Run control program
    controller.run()


if __name__ == "__main__":
    main()
