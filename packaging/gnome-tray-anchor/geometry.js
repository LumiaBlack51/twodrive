// All coordinates are compositor logical pixels, including negative monitors.
export function panelBounds(icon, area) {
    const [ix, iy, iw, ih] = icon;
    const [ax, ay, aw, ah] = area;
    const mx = Math.min(8, Math.max(0, Math.floor((aw - 1) / 2)));
    const my = Math.min(8, Math.max(0, Math.floor((ah - 1) / 2)));
    const width = Math.min(390, Math.max(1, aw - 2 * mx));
    const height = Math.min(570, Math.max(1, ah - 2 * my));
    const center = ix + Math.floor(iw / 2);
    const cy = iy + Math.floor(ih / 2);
    const x = Math.max(ax + mx, Math.min(center - Math.floor(width / 2), ax + aw - width - mx));
    const side = center < ax || center >= ax + aw;
    const y = side ? Math.max(ay + my, Math.min(cy - Math.floor(height / 2), ay + ah - height - my))
        : cy < ay + Math.floor(ah / 2) ? ay + my : ay + ah - height - my;
    return [x, y, width, height];
}

// Actor bounds include a floating/autohide panel even when workarea does not.
export function reservePanel(area, panel, monitor) {
    let [ax, ay, aw, ah] = area;
    const [px, py, pw, ph] = panel;
    const [mx, my, mw, mh] = monitor;
    const right = ax + aw, bottom = ay + ah;
    if (pw >= ph) {
        if (py + ph / 2 < my + mh / 2) ay = Math.min(bottom - 1, Math.max(ay, py + ph));
        else ah = Math.min(bottom, py) - ay;
    } else {
        if (px + pw / 2 < mx + mw / 2) ax = Math.min(right - 1, Math.max(ax, px + pw));
        else aw = Math.min(right, px) - ax;
    }
    return [ax, ay, Math.max(1, Math.min(aw, right - ax)), Math.max(1, Math.min(ah, bottom - ay))];
}

export function ownIndicator(actor, pid) {
    const legacy = actor?._icon;
    if (legacy?.wm_class === 'twodrive-development' && legacy.pid === pid)
        return legacy;
    const sni = actor?._indicator;
    if (sni?.id === 'twodrive-development' &&
        (sni.busName === `org.kde.StatusNotifierItem-${pid}-1` ||
            sni.uniqueId === `org.kde.StatusNotifierItem-${pid}-1`))
        return actor;
    return null;
}
