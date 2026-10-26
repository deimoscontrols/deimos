"""Extract the authoritative PDA panel geometry and artwork without changing it."""
from pathlib import Path
import json
import yaml

root = Path(__file__).resolve().parents[1]
source = root.parent / 'DeimosDaqRev8.pda'
pda = yaml.safe_load(source.read_text())
result = {}
for item in pda['Assembly']['Components']:
    part = item.get('Part', {})
    if part.get('name') not in ('Front Plate', 'Rear Plate'):
        continue
    side = part['name'].split()[0].lower()
    face = part['Components'][0]['PathFace']
    result[side] = face['Attachment2Ds']
    count = 0
    for attachment in result[side]:
        if 'SVG' in attachment:
            count += 1
            path = root / 'reference' / f'{side}_symbol_{count}.svg'
            path.write_text(attachment['SVG']['document'])
            attachment['SVG']['svg_file'] = str(path.relative_to(root))
            del attachment['SVG']['document']
(root / 'reference' / 'pda_panels.json').write_text(json.dumps(result, indent=2))
print('Extracted front and rear cutouts, 111 text objects, and two SVG symbols.')
