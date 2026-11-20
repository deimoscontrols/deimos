"""Export exact-size vector PDFs from the generated SVG artwork."""
from pathlib import Path
from svg_to_pdf import convert

root=Path(__file__).resolve().parents[1]
for side in ('front','rear','lid'):
    for kind in ('engrave','proof'):
        svg=root/'artwork'/f'{side}_{kind}.svg'
        convert(svg,svg.with_suffix('.pdf'))
        print(svg.with_suffix('.pdf'))
