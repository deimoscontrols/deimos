"""A simple program to monitor all peripherals found on the network."""

import os
import time
from pathlib import Path

import deimos
from deimos import LoopMethod, Overflow, LossOfContactPolicy

here = Path(__file__).parent.absolute()

testing = os.environ.get("DEIMOS_TESTING", False)

# Build a new control program to run at 5Hz (the minimum)
# in efficient (OS-scheduled) mode.
c = deimos.Controller("test2", str(here), 5.0)
c.loss_of_contact_policy = LossOfContactPolicy.reconnect_indefinite()
c.loop_method = LoopMethod.efficient()

# Scan for peripherals on the network
peripherals = c.scan()

# Add peripherals to the control program
print("Found peripherals on network:")
for i, p in enumerate(peripherals):
    print(f"  {type(p).__name__} SN {p.serial_number}")
    c.add_peripheral(f"p{i + 1}", p)

# Configure to write data to a CSV file
csv_dispatcher = deimos.dispatcher.CsvDispatcher(1, Overflow.wrap())
c.add_dispatcher("csv", csv_dispatcher)

# Add a calc that runs in-the-loop
five = deimos.calc.Constant(5.0, True)
c.add_calc("five", five)

# Run the control program nonblocking and poll latest values
if len(peripherals) > 0:
    h = c.run_nonblocking()
    try:
        for _ in range(20 if not testing else 1):  # poll for ~4 seconds
            snap = h.read()
            vals = list(snap.values.items())
            print(f"t={snap.timestamp} {snap.system_time} {vals[:3]}")
            time.sleep(0.2)
    finally:
        h.stop()
        try:
            h.join()
        except Exception as e:
            print(f"Run terminated with error: {e}")
