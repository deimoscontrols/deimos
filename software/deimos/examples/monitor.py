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

# Send data to postgres
c.add_timescaledb_dispatcher(
    dbname="postgres",
    host="192.168.8.231:5432",
    user="postgres",
    token_name="POSTGRES_PW_2",
    retention_time_hours=1,
)

# Add a calc that runs in-the-loop
five = deimos.calc.Constant(5.0, True)
c.add_calc("five", five)

# Run the control program
if len(peripherals) > 0:
    c.run()
