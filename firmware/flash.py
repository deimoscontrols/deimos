import os
from pathlib import Path
from subprocess import check_call
import json


def writemac(fp: Path, mac: str):
    # MAC format like 02:00:11:22:33:44
    mac_parts = mac.split(":")
    
    if os.path.exists(fp):
        os.remove(fp)

    mac_bytes = bytes([int(x, base=16) for x in mac_parts])
    
    with open(fp, "wb") as f:
        f.write(mac_bytes)

def writesn(fp: Path, sn: str):
    sn = int(sn)
    sn_bytes = sn.to_bytes(8, "little")

    with open(fp, "wb") as f:
        f.write(sn_bytes)


if __name__ == "__main__":
    here = Path(__file__).parent

    # Load association between probes and boards
    fp = here / "assignments.json"
    with open(fp) as f:
        d = json.load(f)

    # Write the serial number and mac address for each board,
    # then flash it
    for probe, cfg in d.items():
        sn = cfg["sn"]
        mac = cfg["mac"]
        model = cfg["model"]

        scriptfp = here / "flash.sh"
        macfp = here / f"{model}/static/macaddr.in" 
        snfp = here / f"{model}/static/serialnumber.in"

        print(f"Flashing SN {sn} with MAC {mac} on probe {probe}")
        # Write mac address and sn
        writemac(macfp, cfg["mac"])
        writesn(snfp, cfg["sn"])

        # Compile and flash to each probe
        cmd = ["sh", scriptfp, model, probe]
        print("Running", cmd)
        check_call(cmd)
