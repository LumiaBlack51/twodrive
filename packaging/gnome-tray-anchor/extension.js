import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {ownIndicator, panelBounds, reservePanel} from './geometry.js';

const XML = `<node><interface name="app.twodrive.TrayAnchor1">
 <method name="GetAnchor"><arg type="u" direction="in"/>
  <arg type="b" direction="out"/>
  ${'<arg type="i" direction="out"/>'.repeat(8)}
 </method>
 <method name="PositionPanel"><arg type="u" direction="in"/>
  <arg type="b" direction="out"/>
 </method>
 <method name="GetPanel"><arg type="u" direction="in"/>
  <arg type="b" direction="out"/>
  ${'<arg type="i" direction="out"/>'.repeat(4)}
 </method>
</interface></node>`;

export default class TwoDriveTrayAnchor extends Extension {
    enable() {
        this._hooks = new Map();
        this._object = Gio.DBusExportedObject.wrapJSObject(XML, this);
        this._object.export(Gio.DBus.session, '/app/twodrive/TrayAnchor');
        // SNI on native Wayland normally reserves Activate for double-click.
        // Intercept only TwoDrive's primary click, leaving all other icons alone.
        this._timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 500, () => {
            this._watchIndicators();
            return GLib.SOURCE_CONTINUE;
        });
        this._watchIndicators();
    }

    _watchIndicators() {
        for (const actor of Object.values(Main.panel.statusArea)) {
            if (actor?._indicator?.id !== 'twodrive-development' || this._hooks.has(actor))
                continue;
            const signal = actor.connect('button-press-event', (_actor, event) => {
                if (event.get_button() !== Clutter.BUTTON_PRIMARY)
                    return Clutter.EVENT_PROPAGATE;
                actor._indicator.open(...event.get_coords(), event.get_time());
                return Clutter.EVENT_STOP;
            });
            const destroyed = actor.connect('destroy', () => this._hooks.delete(actor));
            this._hooks.set(actor, [signal, destroyed]);
        }
    }

    GetAnchor(pid) {
        for (const actor of Object.values(Main.panel.statusArea)) {
            const icon = ownIndicator(actor, pid);
            if (!icon?.mapped)
                continue;
            const [x, y] = icon.get_transformed_position();
            const [w, h] = icon.get_transformed_size();
            if (w <= 0 || h <= 0)
                continue;
            const monitor = Main.layoutManager.findMonitorForActor(icon);
            if (!monitor)
                continue;
            const area = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
            let usable = [area.x, area.y, area.width, area.height];
            // The dock may be visible while intellihide keeps it out of the
            // workarea. Read only the ancestor bar containing our own icon.
            let bar = null;
            for (let actor = icon.get_parent(); actor && actor !== global.stage; actor = actor.get_parent()) {
                const [px, py] = actor.get_transformed_position();
                const [pw, ph] = actor.get_transformed_size();
                const horizontal = pw >= ph * 2 && ph <= monitor.height / 4;
                const vertical = ph >= pw * 2 && pw <= monitor.width / 4;
                if (pw > 0 && ph > 0 && (horizontal || vertical) &&
                    x >= px && y >= py && x + w <= px + pw + 1 && y + h <= py + ph + 1)
                    bar = [px, py, pw, ph];
            }
            if (bar) usable = reservePanel(usable, bar,
                [monitor.x, monitor.y, monitor.width, monitor.height]);
            return [true, ...[x, y, w, h, ...usable].map(Math.round)];
        }
        return [false, 0, 0, 0, 0, 0, 0, 0, 0];
    }

    PositionPanelAsync([pid], invocation) {
        // The caller may position only its own TwoDrive window. There is no
        // general window-control/evaluation interface or caller-supplied position.
        Gio.DBus.session.call('org.freedesktop.DBus', '/org/freedesktop/DBus',
            'org.freedesktop.DBus', 'GetConnectionUnixProcessID',
            new GLib.Variant('(s)', [invocation.get_sender()]),
            new GLib.VariantType('(u)'), Gio.DBusCallFlags.NONE, 500, null,
            (connection, result) => {
                let positioned = false;
                try {
                    const [caller] = connection.call_finish(result).deep_unpack();
                    const anchor = this.GetAnchor(pid);
                    if (caller === pid && anchor[0]) {
                        const window = this._ownWindow(pid);
                        if (window && !window.decorated) {
                            const rect = panelBounds(anchor.slice(1, 5), anchor.slice(5));
                            window.move_resize_frame(false, ...rect);
                            positioned = true;
                        }
                    }
                } catch (_) {
                    // Host unavailable/changed: fail closed, without touching other windows.
                }
                invocation.return_value(new GLib.Variant('(b)', [positioned]));
            });
    }

    _ownWindow(pid) {
        return global.get_window_actors().map(a => a.meta_window)
            .find(w => w.get_pid() === pid &&
                w.get_wm_class()?.toLowerCase() === 'app.twodrive.twodrive_dav');
    }

    GetPanel(pid) {
        const window = this._ownWindow(pid);
        if (!window || window.is_hidden()) return [false, 0, 0, 0, 0];
        const r = window.get_frame_rect();
        return [true, r.x, r.y, r.width, r.height];
    }

    disable() {
        if (this._timer) GLib.source_remove(this._timer);
        this._timer = 0;
        for (const [actor, signals] of this._hooks)
            for (const signal of signals) actor.disconnect(signal);
        this._hooks.clear();
        this._object?.unexport();
        this._object = null;
    }
}
