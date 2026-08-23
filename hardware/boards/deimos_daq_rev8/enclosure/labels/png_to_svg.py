#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "contourpy>=1.3.0",
#     "numpy>=2.0.0",
# ]
# ///
"""Convert black-on-transparent or black-on-white PNG labels to SVG.

This reads ordinary 8-bit PNGs, turns dark pixels into an ink field, and emits
filled SVG contours. The rectangle output from the first version is still
available as a fallback mode.
"""

from __future__ import annotations

import argparse
import binascii
import math
from dataclasses import dataclass
from pathlib import Path
import struct
import sys
import zlib


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


@dataclass(frozen=True)
class Image:
    width: int
    height: int
    channels: int
    pixels: bytearray


@dataclass
class Rect:
    x: int
    y: int
    width: int
    height: int
    opacity: int


def paeth(left: int, up: int, up_left: int) -> int:
    estimate = left + up - up_left
    left_distance = abs(estimate - left)
    up_distance = abs(estimate - up)
    up_left_distance = abs(estimate - up_left)
    if left_distance <= up_distance and left_distance <= up_left_distance:
        return left
    if up_distance <= up_left_distance:
        return up
    return up_left


def parse_chunks(data: bytes) -> list[tuple[bytes, bytes]]:
    if not data.startswith(PNG_SIGNATURE):
        raise ValueError("not a PNG file")

    position = len(PNG_SIGNATURE)
    chunks: list[tuple[bytes, bytes]] = []
    while position < len(data):
        payload_size = struct.unpack(">I", data[position : position + 4])[0]
        chunk_type = data[position + 4 : position + 8]
        payload = data[position + 8 : position + 8 + payload_size]
        crc = struct.unpack(">I", data[position + 8 + payload_size : position + 12 + payload_size])[0]
        expected_crc = binascii.crc32(chunk_type + payload) & 0xFFFFFFFF
        if crc != expected_crc:
            raise ValueError(f"bad CRC in {chunk_type.decode('ascii', 'replace')} chunk")
        chunks.append((chunk_type, payload))
        position += payload_size + 12
        if chunk_type == b"IEND":
            break

    return chunks


def unfilter(raw: bytes, width: int, height: int, bytes_per_pixel: int) -> bytearray:
    stride = width * bytes_per_pixel
    position = 0
    previous_row = bytearray(stride)
    pixels = bytearray()

    for _ in range(height):
        filter_type = raw[position]
        position += 1
        scanline = raw[position : position + stride]
        position += stride
        row = bytearray(stride)

        for index, value in enumerate(scanline):
            left = row[index - bytes_per_pixel] if index >= bytes_per_pixel else 0
            up = previous_row[index]
            up_left = previous_row[index - bytes_per_pixel] if index >= bytes_per_pixel else 0

            if filter_type == 0:
                reconstructed = value
            elif filter_type == 1:
                reconstructed = value + left
            elif filter_type == 2:
                reconstructed = value + up
            elif filter_type == 3:
                reconstructed = value + ((left + up) // 2)
            elif filter_type == 4:
                reconstructed = value + paeth(left, up, up_left)
            else:
                raise ValueError(f"unsupported PNG filter type {filter_type}")

            row[index] = reconstructed & 0xFF

        pixels.extend(row)
        previous_row = row

    return pixels


def read_png(path: Path) -> Image:
    chunks = parse_chunks(path.read_bytes())
    ihdr = next((payload for chunk_type, payload in chunks if chunk_type == b"IHDR"), None)
    if ihdr is None:
        raise ValueError("missing IHDR chunk")

    width, height, bit_depth, color_type, compression, png_filter, interlace = struct.unpack(
        ">IIBBBBB", ihdr
    )
    if bit_depth != 8 or compression != 0 or png_filter != 0 or interlace != 0:
        raise ValueError("unsupported PNG format; expected 8-bit, non-interlaced PNG")

    channels_by_color_type = {
        0: 1,  # grayscale
        2: 3,  # RGB
        4: 2,  # grayscale + alpha
        6: 4,  # RGBA
    }
    try:
        channels = channels_by_color_type[color_type]
    except KeyError as exc:
        raise ValueError("unsupported PNG color type; expected grayscale, RGB, GA, or RGBA") from exc

    idat_payload = b"".join(payload for chunk_type, payload in chunks if chunk_type == b"IDAT")
    pixels = unfilter(zlib.decompress(idat_payload), width, height, channels)
    return Image(width, height, channels, pixels)


def opacity_at(image: Image, x: int, y: int, levels: int) -> int:
    offset = (y * image.width + x) * image.channels
    if image.channels == 1:
        red = green = blue = image.pixels[offset]
        alpha = 255
    elif image.channels == 2:
        red = green = blue = image.pixels[offset]
        alpha = image.pixels[offset + 1]
    elif image.channels == 3:
        red, green, blue = image.pixels[offset : offset + 3]
        alpha = 255
    else:
        red, green, blue, alpha = image.pixels[offset : offset + 4]

    luma = 0.2126 * red + 0.7152 * green + 0.0722 * blue
    darkness = max(0.0, min(1.0, 1.0 - luma / 255.0))
    opacity = darkness * (alpha / 255.0)
    return round(opacity * levels)


def ink_at(image: Image, x: int, y: int) -> float:
    offset = (y * image.width + x) * image.channels
    if image.channels == 1:
        red = green = blue = image.pixels[offset]
        alpha = 255
    elif image.channels == 2:
        red = green = blue = image.pixels[offset]
        alpha = image.pixels[offset + 1]
    elif image.channels == 3:
        red, green, blue = image.pixels[offset : offset + 3]
        alpha = 255
    else:
        red, green, blue, alpha = image.pixels[offset : offset + 4]

    luma = 0.2126 * red + 0.7152 * green + 0.0722 * blue
    darkness = max(0.0, min(1.0, 1.0 - luma / 255.0))
    return darkness * (alpha / 255.0)


def rects_from_image(image: Image, levels: int) -> list[Rect]:
    rects: list[Rect] = []
    active: dict[tuple[int, int, int], Rect] = {}

    for y in range(image.height):
        row_runs: list[tuple[int, int, int]] = []
        x = 0
        while x < image.width:
            opacity = opacity_at(image, x, y, levels)
            if opacity == 0:
                x += 1
                continue

            start = x
            x += 1
            while x < image.width and opacity_at(image, x, y, levels) == opacity:
                x += 1
            row_runs.append((start, x - start, opacity))

        next_active: dict[tuple[int, int, int], Rect] = {}
        for x, width, opacity in row_runs:
            key = (x, width, opacity)
            rect = active.pop(key, None)
            if rect is None:
                rect = Rect(x=x, y=y, width=width, height=1, opacity=opacity)
            else:
                rect.height += 1
            next_active[key] = rect

        rects.extend(active.values())
        active = next_active

    rects.extend(active.values())
    return rects


def contour_paths_from_image(
    image: Image,
    threshold: float,
    simplify: float,
) -> list[list[tuple[float, float]]]:
    try:
        import contourpy
        import numpy as np
    except ImportError as exc:
        raise RuntimeError(
            "contour mode requires contourpy and numpy; run with `uv run png_to_svg.py ...`"
        ) from exc

    z = np.zeros((image.height + 2, image.width + 2), dtype=np.float64)
    for y in range(image.height):
        for x in range(image.width):
            z[y + 1, x + 1] = ink_at(image, x, y)

    x_coords = np.arange(-0.5, image.width + 1.5, dtype=np.float64)
    y_coords = np.arange(-0.5, image.height + 1.5, dtype=np.float64)
    contour_generator = contourpy.contour_generator(
        x=x_coords,
        y=y_coords,
        z=z,
        name="serial",
    )

    paths: list[list[tuple[float, float]]] = []
    for line in contour_generator.lines(threshold):
        points = [
            (clip(float(x), 0.0, image.width), clip(float(y), 0.0, image.height))
            for x, y in line
        ]
        points = simplify_points(points, simplify)
        if len(points) >= 3:
            paths.append(points)
    return paths


def clip(value: float, lower: float, upper: float) -> float:
    return max(lower, min(upper, value))


def simplify_points(
    points: list[tuple[float, float]],
    tolerance: float,
) -> list[tuple[float, float]]:
    if tolerance <= 0 or len(points) < 4:
        return points

    closed = same_point(points[0], points[-1], tolerance)
    work = points[:-1] if closed else points[:]
    if len(work) < 3:
        return points

    simplified: list[tuple[float, float]] = []
    count = len(work)
    for index, point in enumerate(work):
        previous_point = work[index - 1]
        next_point = work[(index + 1) % count]
        if point_line_distance(point, previous_point, next_point) > tolerance:
            simplified.append(point)

    if closed and simplified:
        simplified.append(simplified[0])
    return simplified


def same_point(a: tuple[float, float], b: tuple[float, float], tolerance: float) -> bool:
    return abs(a[0] - b[0]) <= tolerance and abs(a[1] - b[1]) <= tolerance


def point_line_distance(
    point: tuple[float, float],
    line_start: tuple[float, float],
    line_end: tuple[float, float],
) -> float:
    line_dx = line_end[0] - line_start[0]
    line_dy = line_end[1] - line_start[1]
    length = math.hypot(line_dx, line_dy)
    if length == 0:
        return math.hypot(point[0] - line_start[0], point[1] - line_start[1])
    return abs(
        line_dy * point[0]
        - line_dx * point[1]
        + line_end[0] * line_start[1]
        - line_end[1] * line_start[0]
    ) / length


def opacity_attr(opacity: int, levels: int) -> str:
    if opacity >= levels:
        return ""
    return f' fill-opacity="{opacity / levels:.3f}"'


def svg_escape(value: str) -> str:
    return (
        value.replace("&", "&amp;")
        .replace('"', "&quot;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
    )


def format_number(value: float) -> str:
    if abs(value - round(value)) < 0.001:
        return str(int(round(value)))
    return f"{value:.3f}".rstrip("0").rstrip(".")


def path_data(points: list[tuple[float, float]]) -> str:
    start_x, start_y = points[0]
    commands = [f"M {format_number(start_x)} {format_number(start_y)}"]
    for x, y in points[1:]:
        commands.append(f"L {format_number(x)} {format_number(y)}")
    commands.append("Z")
    return " ".join(commands)


def write_rect_svg(path: Path, image: Image, rects: list[Rect], fill: str, levels: int) -> None:
    lines = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        (
            f'<svg xmlns="http://www.w3.org/2000/svg" '
            f'width="{image.width}" height="{image.height}" '
            f'viewBox="0 0 {image.width} {image.height}">'
        ),
        f'  <g fill="{svg_escape(fill)}">',
    ]
    for rect in rects:
        lines.append(
            f'    <rect x="{rect.x}" y="{rect.y}" width="{rect.width}" '
            f'height="{rect.height}"{opacity_attr(rect.opacity, levels)}/>'
        )
    lines.extend(["  </g>", "</svg>", ""])
    path.write_text("\n".join(lines), encoding="utf-8")


def write_contour_svg(
    path: Path,
    image: Image,
    paths: list[list[tuple[float, float]]],
    fill: str,
) -> None:
    lines = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        (
            f'<svg xmlns="http://www.w3.org/2000/svg" '
            f'width="{image.width}" height="{image.height}" '
            f'viewBox="0 0 {image.width} {image.height}">'
        ),
    ]
    if paths:
        data = " ".join(path_data(points) for points in paths)
        lines.append(f'  <path d="{data}" fill="{svg_escape(fill)}" fill-rule="evenodd"/>')
    lines.extend(["</svg>", ""])
    path.write_text("\n".join(lines), encoding="utf-8")


def output_path_for(input_path: Path, output: Path | None, multiple_inputs: bool) -> Path:
    if output is None:
        return input_path.with_suffix(".svg")
    if multiple_inputs or output.is_dir():
        return output / input_path.with_suffix(".svg").name
    return output


def convert_file(
    input_path: Path,
    output_path: Path,
    fill: str,
    mode: str,
    levels: int,
    threshold: float,
    simplify: float,
) -> tuple[int, int, int]:
    image = read_png(input_path)
    if mode == "rect":
        rects = rects_from_image(image, levels)
        write_rect_svg(output_path, image, rects, fill, levels)
        return image.width, image.height, len(rects)

    paths = contour_paths_from_image(image, threshold, simplify)
    write_contour_svg(output_path, image, paths, fill)
    return image.width, image.height, len(paths)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Convert black-on-transparent or black-on-white PNG labels to SVG "
            "paths. Darker pixels become filled SVG contours."
        )
    )
    parser.add_argument("filename", nargs="+", type=Path)
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        help="output SVG path, or output directory when converting multiple files",
    )
    parser.add_argument("--fill", default="#000000", help="SVG fill color")
    parser.add_argument(
        "--mode",
        choices=("contour", "rect"),
        default="contour",
        help="SVG conversion strategy",
    )
    parser.add_argument(
        "--threshold",
        type=float,
        default=0.5,
        help="ink threshold for contour mode, from 0.0 to 1.0",
    )
    parser.add_argument(
        "--simplify",
        type=float,
        default=0.01,
        help="drop contour points within this many pixels of a straight line",
    )
    parser.add_argument(
        "--levels",
        type=int,
        default=16,
        help="number of opacity levels used for anti-aliased edges",
    )
    args = parser.parse_args()

    if args.levels < 1:
        print("png_to_svg.py: --levels must be at least 1", file=sys.stderr)
        return 1
    if not 0.0 < args.threshold < 1.0:
        print("png_to_svg.py: --threshold must be between 0.0 and 1.0", file=sys.stderr)
        return 1
    if args.simplify < 0:
        print("png_to_svg.py: --simplify must be at least 0", file=sys.stderr)
        return 1
    if args.output is not None and len(args.filename) > 1:
        args.output.mkdir(parents=True, exist_ok=True)

    exit_code = 0
    for filename in args.filename:
        output_path = output_path_for(filename, args.output, len(args.filename) > 1)
        try:
            width, height, rect_count = convert_file(
                filename,
                output_path,
                fill=args.fill,
                mode=args.mode,
                levels=args.levels,
                threshold=args.threshold,
                simplify=args.simplify,
            )
        except Exception as exc:
            print(f"png_to_svg.py: {filename}: {exc}", file=sys.stderr)
            exit_code = 1
            continue
        units = "rects" if args.mode == "rect" else "paths"
        print(f"{filename}: {width}x{height}, {rect_count} {units} -> {output_path}")

    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
