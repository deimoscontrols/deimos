#!/usr/bin/env python3
"""Convert label PNGs from light-on-dark to black-on-transparent."""

from __future__ import annotations

import argparse
import binascii
import collections
from pathlib import Path
import struct
import sys
import zlib


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
RGBA_BYTES_PER_PIXEL = 4


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


def png_chunk(chunk_type: bytes, payload: bytes) -> bytes:
    checksum = binascii.crc32(chunk_type + payload) & 0xFFFFFFFF
    return (
        struct.pack(">I", len(payload))
        + chunk_type
        + payload
        + struct.pack(">I", checksum)
    )


def parse_chunks(data: bytes) -> list[tuple[bytes, bytes]]:
    if not data.startswith(PNG_SIGNATURE):
        raise ValueError("not a PNG file")

    position = len(PNG_SIGNATURE)
    chunks: list[tuple[bytes, bytes]] = []
    while position < len(data):
        payload_size = struct.unpack(">I", data[position : position + 4])[0]
        chunk_type = data[position + 4 : position + 8]
        payload = data[position + 8 : position + 8 + payload_size]
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


def filter_none(pixels: bytearray, width: int, height: int, bytes_per_pixel: int) -> bytes:
    stride = width * bytes_per_pixel
    raw = bytearray()
    for row_index in range(height):
        raw.append(0)
        row_start = row_index * stride
        raw.extend(pixels[row_start : row_start + stride])
    return bytes(raw)


def convert_png(path: Path) -> tuple[int, int, tuple[int, int, int], int, int]:
    chunks = parse_chunks(path.read_bytes())
    ihdr = next((payload for chunk_type, payload in chunks if chunk_type == b"IHDR"), None)
    if ihdr is None:
        raise ValueError("missing IHDR chunk")

    width, height, bit_depth, color_type, compression, png_filter, interlace = struct.unpack(
        ">IIBBBBB", ihdr
    )
    if (bit_depth, color_type, compression, png_filter, interlace) != (8, 6, 0, 0, 0):
        raise ValueError(
            "unsupported PNG format; expected 8-bit RGBA, non-interlaced PNG"
        )

    idat_payload = b"".join(payload for chunk_type, payload in chunks if chunk_type == b"IDAT")
    pixels = unfilter(
        zlib.decompress(idat_payload),
        width,
        height,
        RGBA_BYTES_PER_PIXEL,
    )

    for offset in range(0, len(pixels), RGBA_BYTES_PER_PIXEL):
        pixels[offset] = 255 - pixels[offset]
        pixels[offset + 1] = 255 - pixels[offset + 1]
        pixels[offset + 2] = 255 - pixels[offset + 2]

    colors = collections.Counter(
        tuple(pixels[offset : offset + 3])
        for offset in range(0, len(pixels), RGBA_BYTES_PER_PIXEL)
    )
    background_rgb, background_count = colors.most_common(1)[0]
    background_luma = max(
        1.0,
        0.2126 * background_rgb[0]
        + 0.7152 * background_rgb[1]
        + 0.0722 * background_rgb[2],
    )

    transparent_count = 0
    for offset in range(0, len(pixels), RGBA_BYTES_PER_PIXEL):
        red, green, blue = pixels[offset : offset + 3]
        luma = 0.2126 * red + 0.7152 * green + 0.0722 * blue
        alpha = round(255 * max(0.0, min(1.0, 1 - luma / background_luma)))
        if alpha <= 1:
            alpha = 0
            transparent_count += 1
        pixels[offset : offset + 4] = bytes((0, 0, 0, alpha))

    converted_idat = zlib.compress(
        filter_none(pixels, width, height, RGBA_BYTES_PER_PIXEL),
        level=9,
    )

    output = bytearray(PNG_SIGNATURE)
    wrote_idat = False
    for chunk_type, payload in chunks:
        if chunk_type == b"IDAT":
            if not wrote_idat:
                output.extend(png_chunk(b"IDAT", converted_idat))
                wrote_idat = True
            continue
        if chunk_type == b"IEND":
            output.extend(png_chunk(b"IEND", b""))
        else:
            output.extend(png_chunk(chunk_type, payload))

    path.write_bytes(output)
    return width, height, background_rgb, background_count, transparent_count


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Invert a label PNG and make the light background transparent."
    )
    parser.add_argument("filename", type=Path)
    args = parser.parse_args()

    try:
        width, height, background_rgb, background_count, transparent_count = convert_png(
            args.filename
        )
    except Exception as exc:
        print(f"convert.py: {args.filename}: {exc}", file=sys.stderr)
        return 1

    print(
        f"{args.filename}: {width}x{height}, "
        f"background={background_rgb} ({background_count} px), "
        f"transparent={transparent_count} px"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
