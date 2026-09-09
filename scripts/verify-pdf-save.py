#!/usr/bin/env python3
import os, sys, time, pathlib, tempfile, subprocess, ctypes, shutil
import argparse
parser = argparse.ArgumentParser(description='Verify annotated PDF saves on a mounted TwoDrive using a disposable copy; the copy is uploaded and deleted.')
parser.add_argument('--mount', type=pathlib.Path, required=True)
parser.add_argument('--source', type=pathlib.Path, required=True)
parser.add_argument('--settle-seconds', type=float, default=5)
parser.add_argument('--last-wait-seconds', type=float, default=65)
args = parser.parse_args()
root = pathlib.Path(tempfile.mkdtemp(prefix='twodrive-save-check-'))
mount = args.mount.resolve()
doc = page = None
fd = None
go = ctypes.CDLL('libgobject-2.0.so.0')
go.g_object_unref.argtypes = [ctypes.c_void_p]
try:
    target = mount / ('.twodrive-pdf-save-check-' + root.name + '.pdf')
    shutil.copyfile(args.source, target)
    pop = ctypes.CDLL('libpoppler-glib.so.8')
    pop.poppler_document_new_from_file.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p]
    pop.poppler_document_new_from_file.restype = ctypes.c_void_p
    pop.poppler_document_save.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_void_p]
    pop.poppler_document_save.restype = ctypes.c_int
    doc = pop.poppler_document_new_from_file(target.as_uri().encode(), None, None)
    assert doc

    class Rect(ctypes.Structure):
        _fields_ = [('x1', ctypes.c_double), ('y1', ctypes.c_double), ('x2', ctypes.c_double), ('y2', ctypes.c_double)]
    pop.poppler_document_get_page.argtypes = [ctypes.c_void_p, ctypes.c_int]
    pop.poppler_document_get_page.restype = ctypes.c_void_p
    pop.poppler_annot_text_new.argtypes = [ctypes.c_void_p, ctypes.POINTER(Rect)]
    pop.poppler_annot_text_new.restype = ctypes.c_void_p
    pop.poppler_annot_set_contents.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    pop.poppler_page_add_annot.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    pop.poppler_page_get_annot_mapping.argtypes = [ctypes.c_void_p]
    pop.poppler_page_get_annot_mapping.restype = ctypes.c_void_p
    pop.poppler_page_free_annot_mapping.argtypes = [ctypes.c_void_p]
    glib = ctypes.CDLL('libglib-2.0.so.0')
    glib.g_list_length.argtypes = [ctypes.c_void_p]
    glib.g_list_length.restype = ctypes.c_uint

    def annotation_count(document):
        saved_page = pop.poppler_document_get_page(document, 0)
        mappings = pop.poppler_page_get_annot_mapping(saved_page)
        count = glib.g_list_length(mappings)
        pop.poppler_page_free_annot_mapping(mappings)
        go.g_object_unref(saved_page)
        return count
    baseline = annotation_count(doc)
    page = pop.poppler_document_get_page(doc, 0)
    fd = os.open(target, os.O_RDONLY)
    initial_mtime = os.fstat(fd).st_mtime
    print('Opened before upload completion:', initial_mtime, flush=True)
    for i in range(4):
        delay = args.last_wait_seconds if i == 3 else args.settle_seconds
        print('Waiting for background sync:', delay, flush=True)
        time.sleep(delay)
        os.listdir(mount)
        print('Original, descriptor, path mtimes:', initial_mtime, os.fstat(fd).st_mtime, os.stat(target).st_mtime, flush=True)
        annot = pop.poppler_annot_text_new(doc, ctypes.byref(Rect(20, 20 + i * 20, 40, 40 + i * 20)))
        pop.poppler_annot_set_contents(annot, f'check {i}'.encode())
        pop.poppler_page_add_annot(page, annot)
        go.g_object_unref(annot)
        tmp = root / f'save-{i}.pdf'
        err = ctypes.c_void_p()
        result = pop.poppler_document_save(doc, tmp.as_uri().encode(), ctypes.byref(err))
        if err.value:

            class GError(ctypes.Structure):
                _fields_ = [('domain', ctypes.c_uint), ('code', ctypes.c_int), ('message', ctypes.c_char_p)]
            print('Poppler error:', ctypes.cast(err, ctypes.POINTER(GError)).contents.message, flush=True)
        print('Poppler save', i, bool(result), flush=True)
        if not result:
            raise RuntimeError(f'PDF save {i + 1} failed; outputs: {root}')
        subprocess.run(['gio', 'copy', str(tmp), str(target)], check=True)
        verification = root / f'verified-{i}.pdf'
        shutil.copyfile(target, verification)
        saved_doc = pop.poppler_document_new_from_file(verification.as_uri().encode(), None, None)
        if not saved_doc:
            raise RuntimeError('Saved PDF could not be reopened')
        count = annotation_count(saved_doc)
        go.g_object_unref(saved_doc)
        assert count >= baseline + i + 1, 'Saved annotation missing'
        print('Persisted annotation count:', count, flush=True)
        try:
            print('old descriptor fstat:', os.fstat(fd).st_size, flush=True)
        except OSError as e:
            print('old descriptor fstat:', e, flush=True)
    print('PASS: all four annotated saves completed', flush=True)
finally:
    if fd is not None:
        os.close(fd)
    if page:
        go.g_object_unref(page)
    if doc:
        go.g_object_unref(doc)
    if 'target' in locals() and target.exists():
        for attempt in range(10):
            try:
                target.unlink()
                break
            except OSError:
                if attempt == 9:
                    raise
                time.sleep(1)
    print('Local test outputs:', root)
