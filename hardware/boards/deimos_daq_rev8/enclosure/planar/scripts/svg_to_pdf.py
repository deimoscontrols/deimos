"""Write exact-size vector PDFs from this project's generated SVG subset.

The exporter accepts absolute M/L/H/Z paths and rectangles. It rejects other
geometry instead of silently dropping it. It needs only the Python standard library.
"""
from pathlib import Path
import re
import xml.etree.ElementTree as ET
import zlib


def color(value):
    names={'black':'#000000','white':'#ffffff'}
    value=names.get(value,value)
    if not value.startswith('#'):raise ValueError(f'Unsupported color: {value}')
    value=value[1:]
    if len(value)==3:value=''.join(x*2 for x in value)
    return ' '.join(f'{int(value[i:i+2],16)/255:.7f}' for i in (0,2,4))


def path_commands(data):
    tokens=re.findall(r'[A-Za-z]|[-+]?(?:\d*\.\d+|\d+\.?\d*)(?:[eE][-+]?\d+)?',data)
    result=[];i=0;command=None;x=y=0
    while i<len(tokens):
        if tokens[i].isalpha():
            command=tokens[i];i+=1
        if command in ('M','L'):
            x=float(tokens[i]);y=float(tokens[i+1]);i+=2
            result.append(f'{x:.6f} {y:.6f} '+('m' if command=='M' else 'l'))
            if command=='M':command='L'
        elif command=='H':
            x=float(tokens[i]);i+=1;result.append(f'{x:.6f} {y:.6f} l')
        elif command=='Z':
            result.append('h');command=None
        else:raise ValueError(f'Unsupported SVG path command: {command}')
    return '\n'.join(result)


def convert(svg, pdf):
    root=ET.parse(svg).getroot()
    xmin,ymin,width,height=map(float,root.attrib['viewBox'].split())
    if abs(float(root.attrib['width'].removesuffix('mm'))-width)>1e-6:
        raise ValueError('SVG must use one drawing unit per millimeter.')
    factor=72/25.4
    stream=[f'{factor:.12f} 0 0 {-factor:.12f} {-xmin*factor:.12f} {(ymin+height)*factor:.12f} cm']
    for element in root:
        kind=element.tag.rsplit('}',1)[-1]
        attr=element.attrib
        stream.append('q')
        fill=attr.get('fill','black');stroke=attr.get('stroke','none')
        if fill!='none':stream.append(color(fill)+' rg')
        if stroke!='none':
            stream.append(color(stroke)+' RG')
            stream.append(attr.get('stroke-width','1')+' w')
            if 'stroke-dasharray' in attr:stream.append('['+attr['stroke-dasharray'].replace(',',' ')+'] 0 d')
        if kind=='path':stream.append(path_commands(attr['d']))
        elif kind=='rect':stream.append(' '.join(attr.get(k,'0') for k in ['x','y','width','height'])+' re')
        else:raise ValueError(f'Unsupported SVG element: {kind}')
        op='B' if fill!='none' and stroke!='none' else 'f' if fill!='none' else 'S' if stroke!='none' else 'n'
        if attr.get('fill-rule')=='evenodd' and op in ('f','B'):op+='*'
        stream.extend([op,'Q'])
    content=zlib.compress(('\n'.join(stream)+'\n').encode('ascii'))
    objects=[
        b'<< /Type /Catalog /Pages 2 0 R >>',
        b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
        f'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width*factor:.10f} {height*factor:.10f}] /Resources << >> /Contents 4 0 R >>'.encode(),
        f'<< /Length {len(content)} /Filter /FlateDecode >>\nstream\n'.encode()+content+b'\nendstream',
    ]
    output=bytearray(b'%PDF-1.4\n%\xe2\xe3\xcf\xd3\n');offsets=[0]
    for number,obj in enumerate(objects,1):
        offsets.append(len(output));output.extend(f'{number} 0 obj\n'.encode()+obj+b'\nendobj\n')
    xref=len(output)
    output.extend(f'xref\n0 {len(objects)+1}\n0000000000 65535 f \n'.encode())
    for offset in offsets[1:]:output.extend(f'{offset:010d} 00000 n \n'.encode())
    output.extend(f'trailer\n<< /Size {len(objects)+1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode())
    Path(pdf).write_bytes(output)
