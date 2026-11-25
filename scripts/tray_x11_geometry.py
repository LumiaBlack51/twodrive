"""The tray content rectangle in X11 root pixels, inside any GTK shadow."""
import re
import subprocess


def panel_geometry(window):
    info = subprocess.check_output(["xwininfo", "-id", window], text=True)
    x, y, width, height = (
        int(re.search(label + r":\s+(-?\d+)", info).group(1))
        for label in ("Absolute upper-left X", "Absolute upper-left Y", "Width", "Height")
    )
    prop = subprocess.check_output(["xprop", "-id", window, "_GTK_FRAME_EXTENTS"], text=True)
    if "=" in prop:
        left, right, top, bottom = map(int, prop.split("=", 1)[1].split(","))
        return x + left, y + top, width - left - right, height - top - bottom
    return x, y, width, height
