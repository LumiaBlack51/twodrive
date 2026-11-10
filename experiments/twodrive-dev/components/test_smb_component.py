"""Adversarial component/unit semantics; these are not server interoperability tests."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch, Mock

spec = importlib.util.spec_from_file_location('smb_component', Path(os.environ.get('TWODRIVE_TEST_SMB_COMPONENT', Path(__file__).with_name('smb_readonly.py'))))
component = importlib.util.module_from_spec(spec)
spec.loader.exec_module(component)


class ComponentTests(unittest.TestCase):
    def test_names_cannot_inject_paths_streams_or_win32_aliases(self):
        for value in ['../x', '..', '.', 'a/b', 'a\\b', 'x:ads', 'CON.txt', 'nul', 'LPT1', 'x.', 'x ', 'x\n', 'x\0']:
            with self.subTest(value=value), self.assertRaises(ValueError): component.component(value)
        for value in ['你好 space.txt', 'Mixed Case', 'é', 'COM10']: self.assertEqual(component.component(value),value)
        for value in ['relative', '/a//b', '/a/../x', '/x/']:
            with self.assertRaises(ValueError): component.parts(value)

    def test_real_library_integer_filetime_and_directory_datetime_have_matching_observation(self):
        from smbprotocol.file_info import FileBasicInformation, FileStandardInformation, FileInternalInformation, FileIdFullDirectoryInformation
        from datetime import datetime, timezone
        basic, standard, internal = FileBasicInformation(), FileStandardInformation(), FileInternalInformation()
        ticks = 134385123662924571  # deliberately includes sub-microsecond precision
        basic['last_write_time'] = ticks
        basic['change_time'] = ticks
        standard['end_of_file'] = 22
        internal['index_number'] = 7
        def query(_, cls):
            return {FileBasicInformation:basic, FileStandardInformation:standard, FileInternalInformation:internal}[cls]
        with patch.object(component,'query',query):
            result=component.snapshot(Mock())
        self.assertEqual(result['modified'],(ticks-116444736000000000)//10000000)
        # DirectoryInformation decodes 100ns ticks into Python microseconds;
        # compare at the common precision, without treating it as a strong version.
        row=FileIdFullDirectoryInformation()
        row['change_time']=datetime.fromtimestamp((ticks-116444736000000000)//10000000,tz=timezone.utc).replace(microsecond=(ticks//10)%1000000)
        self.assertEqual(result['change'],str(int.from_bytes(row['change_time'].pack(),'little')//10))
        basic['file_attributes'] = 0x400
        with patch.object(component,'query',query), self.assertRaisesRegex(ValueError,'link_rejected'):
            component.snapshot(Mock())

    def test_all_ancestor_handles_deny_write_delete_and_open_reparse_itself(self):
        opened=[]
        class Handle:
            def __init__(self,_,name): self.name=name
            def create(self,impersonation,access,attrs,share,disposition,options):
                self_case.assertEqual(access,0x81)
                self_case.assertEqual(share,1)
                self_case.assertEqual(disposition,1)
                self_case.assertTrue(options & 0x200000)
                opened.append(self.name)
            def close(self): closed.append(self.name)
        closed=[]
        self_case=self
        with patch('smbprotocol.open.Open',Handle), patch.object(component,'require_reparse_support'), patch.object(component,'snapshot',return_value={'directory':True}):
            with component.ExitStack() as stack:
                component.open_safe(Mock(),['root','child'],stack)
                self.assertEqual(closed,[])
            self.assertEqual(opened,['','root','root\\child'])
            self.assertEqual(closed,list(reversed(opened)))
        opened.clear()
        with patch('smbprotocol.open.Open',Handle), patch.object(component,'require_reparse_support'), patch.object(component,'snapshot',side_effect=[{'directory':True},ValueError('link_rejected')]):
            with self.assertRaisesRegex(ValueError,'link_rejected'), component.ExitStack() as stack:
                component.open_safe(Mock(),['root','child'],stack)
        self.assertEqual(opened,['','root'])

    def test_server_without_reparse_safety_is_rejected_before_traversal(self):
        with patch.object(component,'query',return_value={'attributes':Mock(get_value=lambda:0)}):
            with self.assertRaisesRegex(ValueError,'reparse_safety_unavailable'):
                component.require_reparse_support(Mock())
        with patch.object(component,'query',return_value={'attributes':Mock(get_value=lambda:0x80)}):
            component.require_reparse_support(Mock())

    def test_low_level_receive_cannot_follow_symlink_even_when_caller_requests_it(self):
        from smbprotocol.connection import Connection
        import uuid
        # The fallback is precisely the old component's connection constructor,
        # so an old-file run exercises its real upstream default, not a missing API.
        factory=getattr(component,'confined_connection',
                        lambda server,port:Connection(uuid.uuid4(),server,port=port,require_signing=True))
        connection=factory('127.0.0.1',445)
        request=Mock()
        with patch.object(Connection,'receive',autospec=True,return_value='response') as receive:
            self.assertEqual(connection.receive(request,resolve_symlinks=True),'response')
            self.assertIs(receive.call_args.kwargs.get('resolve_symlinks'),False)
        self.assertTrue(connection.require_signing)

    def test_unsupported_write_cannot_connect_or_change_remote(self):
        with patch('smbprotocol.connection.Connection',side_effect=AssertionError('network used')):
            with self.assertRaisesRegex(ValueError,'read_only'):
                component.run({'op':'upload','path':'/x','root':'/','share':'synthetic'})

    def test_exception_messages_and_malformed_inputs_are_redacted(self):
        for raw in ['malformed private-password',json.dumps({'op':'stat'})]:
            output=io.StringIO()
            with patch('sys.stdin',Mock(buffer=io.BytesIO((raw+'\n').encode()))),contextlib.redirect_stdout(output):
                self.assertEqual(component.main(),1)
            result=json.loads(output.getvalue())
            self.assertEqual(result['type'],'error')
            self.assertNotIn('private-password',output.getvalue())
            self.assertNotIn('Traceback',output.getvalue())
        with patch.object(component,'run',side_effect=RuntimeError('secret-password private-path')):
            output=io.StringIO()
            with patch('sys.stdin',Mock(buffer=io.BytesIO(b'{}\n'))),contextlib.redirect_stdout(output):component.main()
            self.assertNotIn('secret-password',output.getvalue())
            self.assertNotIn('private-path',output.getvalue())


if __name__ == '__main__': unittest.main()
