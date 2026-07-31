"""Build and flash assigned firmware identities onto attached DAQ probes."""

import json
import re
from pathlib import Path
from shutil import copyfile
from subprocess import check_call


CALIBRATION_RECORD_MODELS = {
    "deimos_daq_rev7": "DeimosDaqRev7",
}


def writemac(fp: Path, mac: str):
    """Write one assigned MAC address as the firmware's six-byte image.

    Args:
        fp: Destination binary-file path.
        mac: MAC address in colon- or hyphen-separated hexadecimal notation.

    Raises:
        ValueError: The address is not six two-digit hexadecimal octets.
    """
    mac_parts = re.split("[:-]", mac)
    if len(mac_parts) != 6 or any(len(part) != 2 for part in mac_parts):
        raise ValueError(f"Invalid MAC address: {mac}")

    try:
        mac_bytes = bytes(int(part, base=16) for part in mac_parts)
    except ValueError as error:
        raise ValueError(f"Invalid MAC address: {mac}") from error

    with open(fp, "wb") as f:
        f.write(mac_bytes)


def writesn(fp: Path, sn: str):
    """Write one assigned serial number as an eight-byte little-endian integer.

    Args:
        fp: Destination binary-file path.
        sn: Nonnegative base-10 serial-number text.
    """
    sn = int(sn)
    sn_bytes = sn.to_bytes(8, "little")

    with open(fp, "wb") as f:
        f.write(sn_bytes)


def stage_calibration(source: Path, destination: Path) -> bool:
    """Stage one unit's generated calibration for firmware inclusion.

    A missing source selects the firmware's identity calibration. Removing an
    existing destination is necessary because all units of a model share the
    same build input path when multiple probes are flashed in one invocation.

    Args:
        source: Unit-specific generated `calibration.bin` path.
        destination: Model-specific firmware `static/calibration.in` path.

    Returns:
        Whether a generated calibration was staged.
    """
    if source.is_file():
        copyfile(source, destination)
        return True

    destination.unlink(missing_ok=True)
    return False


if __name__ == "__main__":
    here = Path(__file__).parent

    # Load association between probes and boards
    fp = here / "assignments.json"
    with open(fp) as f:
        d = json.load(f)

    probe_to_usb_device_map = d["probe_to_usb_device_map"]
    probe_to_daq_map = d["probe_to_daq_map"]

    # Write the serial number and mac address for each board,
    # then flash it
    for probe, cfg in probe_to_daq_map.items():
        sn = cfg["sn"]
        mac = cfg["mac"]
        model = cfg["model"]

        scriptfp = here / "flash.sh"
        macfp = here / f"{model}/static/macaddr.in"
        snfp = here / f"{model}/static/serialnumber.in"

        calibration_record_model = CALIBRATION_RECORD_MODELS.get(model)
        if calibration_record_model is not None:
            calibration_source = (
                here.parent
                / "software"
                / "deimos_website"
                / "docs"
                / "records"
                / calibration_record_model
                / str(sn)
                / "calibration.bin"
            )
            calibration_destination = here / model / "static" / "calibration.in"
            if stage_calibration(calibration_source, calibration_destination):
                print(f"Using calibration {calibration_source}")
            else:
                print(
                    f"No calibration found at {calibration_source}; "
                    "using the identity calibration"
                )

        print(f"Flashing SN {sn} with MAC {mac} on probe {probe}")
        # Write mac address and sn
        writemac(macfp, cfg["mac"])
        writesn(snfp, cfg["sn"])

        # Compile and flash to each probe
        probe_usb_device = probe_to_usb_device_map[probe]
        cmd = ["sh", scriptfp, model, probe_usb_device]
        print("Running", cmd)
        check_call(cmd, cwd=here)
