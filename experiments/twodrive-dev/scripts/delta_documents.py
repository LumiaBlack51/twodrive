"""Synthetic format fixtures and a native CLI delta check (Python stdlib only).

These are generated containers, not recordings of Microsoft Office/editor saves.
Every byte crosses the existing authenticated QUIC gateway in smoke.py.
"""
import io
import random
import struct
import subprocess
import zipfile
import zlib


def png(pixels):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    rows = b''.join(b'\0' + pixels[n:n+1536] for n in range(0, len(pixels), 1536))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 512, 512, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


def pdf(pixels, text):
    image = zlib.compress(pixels)
    content = f'BT /F1 20 Tf 30 760 Td ({text}) Tj ET q 500 0 0 500 30 100 cm /Im1 Do Q'.encode()
    objects = [b'<< /Type /Catalog /Pages 2 0 R >>',
        b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
        b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 4 0 R >> /XObject << /Im1 5 0 R >> >> /Contents 6 0 R >>',
        b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
        f'<< /Type /XObject /Subtype /Image /Width 512 /Height 512 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {len(image)} >>\nstream\n'.encode() + image + b'\nendstream',
        f'<< /Length {len(content)} >>\nstream\n'.encode() + content + b'\nendstream']
    output = bytearray(b'%PDF-1.4\n%\xe2\xe3\xcf\xd3\n')
    offsets = [0]
    for number, obj in enumerate(objects, 1):
        offsets.append(len(output))
        output += f'{number} 0 obj\n'.encode() + obj + b'\nendobj\n'
    start = len(output)
    output += f'xref\n0 {len(offsets)}\n0000000000 65535 f \n'.encode()
    for offset in offsets[1:]:
        output += f'{offset:010d} 00000 n \n'.encode()
    output += f'trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n'.encode()
    return bytes(output)


def office(kind, image, text):
    rel = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships/'
    if kind == 'docx':
        main = 'word/document.xml'
        mime = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'
        parts = {main: f'<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>',
                 'word/media/image1.png': image}
    else:
        main = 'ppt/presentation.xml'
        mime = 'application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml'
        parts = {main: '<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>',
            'ppt/slides/slide1.xml': f'<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld name="{text}"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>',
            'ppt/_rels/presentation.xml.rels': f'<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}slide" Target="slides/slide1.xml"/></Relationships>',
            'ppt/media/image1.png': image}
    extra = '' if kind == 'docx' else '<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>'
    parts['[Content_Types].xml'] = f'<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/{main}" ContentType="{mime}"/>{extra}</Types>'
    parts['_rels/.rels'] = f'<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}officeDocument" Target="{main}"/></Relationships>'
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in parts.items():
            item = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
            item.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(item, data)
    with zipfile.ZipFile(io.BytesIO(output.getvalue())) as archive:
        assert archive.testzip() is None
    return output.getvalue()


def check_documents(binary, sandbox, root, url, credentials, request):
    pixels = random.Random(20261029).randbytes(512 * 512 * 3)
    image = png(pixels)
    for kind in ['pdf', 'docx', 'pptx']:
        build = (lambda text: pdf(pixels, text)) if kind == 'pdf' else (lambda text: office(kind, image, text))
        original = build('Before')
        edited = build('After: inserted content and changed container offsets')
        name = f'delta-sample.{kind}'
        (root / name).write_bytes(original)
        source = sandbox / f'edited.{kind}'
        source.write_bytes(edited)
        # CLI list obtains the same version as a file manager's metadata query.
        prefix = [str(binary), '--state', str(sandbox / 'upload-client'), 'webdav', '--url', url,
                  '--auth-file', str(credentials), '--write']
        def etag():
            output = subprocess.check_output(prefix + ['list'], text=True, encoding='utf-8', timeout=30)
            return next(line.split('\t')[2] for line in output.splitlines() if line.endswith('/' + name))
        output = subprocess.check_output(prefix + ['put', str(source), '/' + name, '--if-match', etag()], text=True, encoding='utf-8', timeout=60).strip()
        values = dict(value.split('=', 1) for value in output.split())
        assert values['upload'] == 'delta', output
        assert int(values['bytes']) + int(values['signature']) < len(edited) // 4, output
        assert request(name) == edited and (root / name).read_bytes() == edited
        full = subprocess.check_output(prefix + ['--full-upload', 'put', str(source), '/' + name, '--if-match', etag()], text=True, encoding='utf-8', timeout=60).strip()
        assert full.startswith(f'upload=full bytes={len(edited)} '), full
        assert request(name) == edited
        print(f'PASS: synthetic {kind.upper()} delta and forced full; file={len(edited)} {output}', flush=True)
        request(name, 'DELETE')
