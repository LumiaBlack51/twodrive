"""Private one-request SMB2/3 component. stdout is bounded NDJSON, never logs.

Use low-level smbprotocol only: no smbclient pool, referrals, link following or
implicit OS credentials. Parent owns cancellation by terminating this process.
"""
import base64
import hashlib
import importlib.metadata
import json
import logging
import os
import sys
import uuid
from contextlib import ExitStack

MAX_ENTRIES = 10000
CHUNK = 65536


def component(name):
    if (not name or name in ('.', '..') or len(name.encode('utf-16-le')) > 510
            or name[-1] in ' .' or any(c in '\\/:*?"<>|' or ord(c) < 32 for c in name)
            or name.split('.')[0].upper() in {'CON', 'PRN', 'AUX', 'NUL',
                *('COM' + str(i) for i in range(1, 10)),
                *('LPT' + str(i) for i in range(1, 10))}):
        raise ValueError('unsafe_name')
    return name


def parts(path):
    if not isinstance(path, str) or not path.startswith('/') or len(path) > 4096:
        raise ValueError('unsafe_path')
    return [] if path == '/' else [component(n) for n in path[1:].split('/')]


def emit(value):
    print(json.dumps(value, ensure_ascii=True, separators=(',', ':')), flush=True)


def query(handle, cls):
    from smbprotocol.open import SMB2QueryInfoRequest, SMB2QueryInfoResponse
    info = cls()
    req = SMB2QueryInfoRequest()
    req['info_type'] = info.INFO_TYPE
    req['file_info_class'] = info.INFO_CLASS
    req['file_id'] = handle.file_id
    req['output_buffer_length'] = 65536
    request = handle.connection.send(req, handle.tree_connect.session.session_id,
                                     handle.tree_connect.tree_connect_id)
    reply = handle.connection.receive(request)
    response = SMB2QueryInfoResponse()
    response.unpack(reply['data'].get_value())
    return response.parse_buffer(cls)


def require_reparse_support(handle):
    from collections import OrderedDict
    from smbprotocol.structure import Structure, IntField, BytesField

    class FileFsAttributeInformation(Structure):
        INFO_TYPE = 2  # SMB2_0_INFO_FILESYSTEM
        INFO_CLASS = 5  # FileFsAttributeInformation

        def __init__(self):
            self.fields = OrderedDict([
                ('attributes', IntField(size=4)),
                ('maximum_component_name_length', IntField(size=4)),
                ('name_length', IntField(size=4)),
                ('name', BytesField(size=lambda obj: obj['name_length'].get_value())),
            ])
            super().__init__()

    attributes = query(handle, FileFsAttributeInformation)['attributes'].get_value()
    if not attributes & 0x80:  # FILE_SUPPORTS_REPARSE_POINTS
        raise ValueError('reparse_safety_unavailable')


def snapshot(handle):
    from smbprotocol.file_info import FileBasicInformation, FileStandardInformation, FileInternalInformation
    basic, standard = query(handle, FileBasicInformation), query(handle, FileStandardInformation)
    attrs = basic['file_attributes'].get_value()
    if attrs & 0x400:  # FILE_ATTRIBUTE_REPARSE_POINT
        raise ValueError('link_rejected')
    return dict(directory=bool(attrs & 0x10), size=standard['end_of_file'].get_value(),
                modified=(basic['last_write_time'].get_value() - 116444736000000000) // 10000000,
                change=str(basic['change_time'].get_value() // 10),
                key=str(query(handle, FileInternalInformation)['index_number'].get_value()))


def checked_open(tree, name, stack, directory=False, volume=False):
    from smbprotocol.open import Open, ImpersonationLevel, ShareAccess, CreateDisposition, CreateOptions
    handle = Open(tree, name)
    options = CreateOptions.FILE_OPEN_REPARSE_POINT
    if directory:
        options |= CreateOptions.FILE_DIRECTORY_FILE
    handle.create(ImpersonationLevel.Impersonation, 0x81, 0,
                  ShareAccess.FILE_SHARE_READ, CreateDisposition.FILE_OPEN, options)
    stack.callback(handle.close)
    if volume:
        require_reparse_support(handle)
    return handle, snapshot(handle)


def open_safe(tree, names, stack):
    # Keep every ancestor open without WRITE/DELETE sharing until completion.
    # FILE_OPEN_REPARSE_POINT applies to the final component of each prefix.
    for i in range(len(names) + 1):
        handle, result = checked_open(tree, '\\'.join(names[:i]), stack,
                                      directory=i < len(names) or not names, volume=i == 0)
        if i < len(names) and not result['directory']:
            raise ValueError('unsafe_path')
    return handle, result


def confined_connection(server, port):
    from smbprotocol.connection import Connection

    class NoFollowConnection(Connection):
        # Low-level Open also calls Connection.receive(), whose upstream default
        # may retry STATUS_STOPPED_ON_SYMLINK within the share. Force refusal for
        # every call, including calls made inside the library itself.
        def receive(self, request, wait=True, timeout=None, resolve_symlinks=True):
            return super().receive(request, wait=wait, timeout=timeout, resolve_symlinks=False)

    return NoFollowConnection(uuid.uuid4(), server, port=port, require_signing=True)


def run(req):
    if importlib.metadata.version('smbprotocol') != '1.17.0':
        raise ValueError('component_version')
    from smbprotocol import Dialects
    from smbprotocol.session import Session
    from smbprotocol.tree import TreeConnect
    from smbprotocol.file_info import FileInformationClass
    from smbprotocol.exceptions import NoMoreFiles, ObjectNameNotFound, ObjectPathNotFound
    names = parts(req['root']) + parts(req['path'])
    component(req['share'])
    op = req['op']
    if op not in ('list', 'stat', 'download'):
        raise ValueError('read_only')
    conn = confined_connection(req['server'], req['port'])
    try:
        conn.connect(timeout=15)
        if conn.dialect < Dialects.SMB_2_0_2:
            raise ValueError('dialect_rejected')
        guid = str(conn.server_guid)
        if req.get('server_guid') and req['server_guid'] != guid:
            raise ValueError('server_changed')
        user = (req['domain'] + '\\' if req['domain'] else '') + req['username']
        session = Session(conn, username=user, password=req['password'],
                          require_encryption=req['encrypt'], auth_protocol='ntlm')
        session.connect()
        tree = TreeConnect(session, '\\\\' + req['server'] + '\\' + req['share'])
        tree.connect()  # secure negotiate validation stays enabled
        if tree.is_dfs_share:
            raise ValueError('dfs_rejected')
        emit(dict(type='hello', server_guid=guid, dialect=conn.dialect,
                  encrypted=bool(session.encrypt_data or tree.encrypt_data)))
        with ExitStack() as stack:
            try:
                handle, info = open_safe(tree, names, stack)
            except (ObjectNameNotFound, ObjectPathNotFound):
                if op != 'stat':
                    raise
                emit(dict(type='end', missing=True))
                return
            if op == 'stat':
                emit(dict(type='entry', path=req['path'], **info))
            elif op == 'list':
                if not info['directory']:
                    raise ValueError('not_directory')
                seen = set()
                pages = 0
                while True:
                    pages += 1
                    if pages > MAX_ENTRIES + 1:
                        raise ValueError('directory_budget')
                    try:
                        rows = handle.query_directory('*', FileInformationClass.FILE_ID_FULL_DIRECTORY_INFORMATION,
                                                      max_output=CHUNK)
                    except NoMoreFiles:
                        break
                    if not rows:
                        raise ValueError('incomplete_directory')
                    for row in rows:
                        name = row['file_name'].get_value().decode('utf-16-le', errors='strict')
                        if name in ('.', '..'):
                            continue
                        component(name)
                        if name in seen or len(seen) >= MAX_ENTRIES:
                            raise ValueError('directory_budget')
                        seen.add(name)
                        # Reject the entire listing, never turn unsafe rows into paths.
                        if row['file_attributes'].get_value() & 0x400:
                            raise ValueError('link_rejected')
                        path = req['path'].rstrip('/') + '/' + name
                        # A listing may synthesize target metadata for a Unix
                        # link. Validate each child with no-follow while its
                        # already-checked ancestors remain locked; publish only
                        # the checked handle's metadata, never the raw row.
                        with ExitStack() as child_stack:
                            _, child = checked_open(tree, '\\'.join(names + [name]), child_stack)
                            emit(dict(type='entry', path=path, **child))
            else:
                if info['directory']:
                    raise ValueError('not_file')
                expected = req['expected']
                if any(info[k] != expected[k] for k in ('key', 'size', 'change')):
                    raise ValueError('object_changed')
                # Share deny-write/delete supplies a transfer-local snapshot; no
                # path/mtime/size tuple is presented as a strong reconnect version.
                offset, digest = 0, hashlib.sha256()
                while offset < info['size']:
                    data = handle.read(offset, min(CHUNK, conn.max_read_size, info['size'] - offset))
                    if not data:
                        raise ValueError('truncated')
                    digest.update(data)
                    offset += len(data)
                    emit(dict(type='data', data=base64.b64encode(data).decode('ascii')))
                if snapshot(handle) != info:
                    raise ValueError('object_changed')
                emit(dict(type='verified', size=offset, sha256=digest.hexdigest()))
        # End only after all handles close successfully. Any disconnect/close
        # failure is a failed transfer, even if every byte was already sent.
        tree.disconnect()
        session.disconnect()
        emit(dict(type='end'))
    finally:
        conn.disconnect(close=False)


def main():
    logging.disable(logging.CRITICAL)
    os.environ["LM_COMPAT_LEVEL"] = "5"  # Python NTLM: no LM/NTLMv1 fallback
    try:
        line = sys.stdin.buffer.readline(65537)
        if len(line) > 65536:
            raise ValueError('request_budget')
        run(json.loads(line))
        return 0
    except Exception as exc:
        # Do not serialize exception messages, paths, credentials or tracebacks.
        status = getattr(exc, 'status', None)
        code = 'protocol_error'
        if isinstance(exc, ImportError):
            code = 'component_missing'
        elif isinstance(exc, ValueError) and str(exc) in {
            'unsafe_name', 'unsafe_path', 'read_only', 'component_version',
            'link_rejected', 'dfs_rejected', 'reparse_safety_unavailable', 'server_changed', 'dialect_rejected',
            'directory_budget', 'incomplete_directory', 'not_directory',
            'not_file', 'object_changed', 'truncated'}:
            code = str(exc)
        emit(dict(type='error', code=code, status=status if isinstance(status, int) else None))
        return 1


if __name__ == '__main__':
    sys.exit(main())
