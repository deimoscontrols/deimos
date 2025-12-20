"""A simple program to monitor all peripherals found on the network"""

from pathlib import Path

import deimos

here = Path(__file__).parent.absolute()

# Build a new control program to run at 5Hz
c = deimos.Controller("test2", str(here), 5.0)

# Scan for peripherals on the network
peripherals = c.scan() 

# Add peripherals to the control program
print("Found peripherals on network:")
for i, p in enumerate(peripherals):
    print(f"  {type(p).__name__} SN {p.serial_number}")
    c.add_peripheral(f"p{i + 1}", p)

# Configure to write data to a CSV file
c.add_csv_dispatcher()

# Add a calc that runs in-the-loop
five = deimos.calc.Constant(5.0, True)
c.add_calc("five", five)

# Run the control program nonblocking and poll latest values
if len(peripherals) > 0:
    h = c.run_nonblocking()
    try:
        import time

        for _ in range(20):  # poll for ~4 seconds
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
