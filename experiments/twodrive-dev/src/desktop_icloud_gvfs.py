"""Mount the worker's authenticated loopback endpoint in the GNOME session."""
import json
import sys
from urllib.parse import urlsplit

from gi.repository import Gio, GLib

mode, uri = sys.argv[1:]
location = Gio.File.new_for_uri(uri)
loop = GLib.MainLoop()
cancel = Gio.Cancellable()
GLib.timeout_add_seconds(8, lambda: (cancel.cancel(), False)[1])
errors = []


def finish(fn):
    def callback(source, result):
        try:
            fn(source, result)
        except GLib.Error as error:
            errors.append(error)
        loop.quit()
    return callback


if mode == "mount":
    secret = sys.stdin.readline().rstrip("\n")
    operation = Gio.MountOperation()

    def ask(op, _message, _user, _domain, _flags):
        op.set_username(urlsplit(uri).username)
        op.set_password(secret)
        op.set_password_save(Gio.PasswordSave.NEVER)
        op.reply(Gio.MountOperationResult.HANDLED)

    operation.connect("ask-password", ask)
    location.mount_enclosing_volume(
        Gio.MountMountFlags.NONE, operation, cancel,
        finish(lambda source, result: source.mount_enclosing_volume_finish(result)),
    )
    loop.run()
    if errors:
        raise SystemExit(1)
    # Use the actual GVfs export, which is available to POSIX applications.
    print(json.dumps(location.get_path()))
elif mode == "unmount":
    try:
        mount = location.find_enclosing_mount(cancel)
    except GLib.Error as error:
        raise SystemExit(0 if error.matches(Gio.io_error_quark(), Gio.IOErrorEnum.NOT_MOUNTED) else 1)
    mount.unmount_with_operation(
        Gio.MountUnmountFlags.NONE, None, cancel,
        finish(lambda source, result: source.unmount_with_operation_finish(result)),
    )
    loop.run()
    if errors:
        raise SystemExit(1)
else:
    raise SystemExit(1)
