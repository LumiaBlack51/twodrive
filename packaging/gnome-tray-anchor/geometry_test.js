import {ownIndicator, panelBounds, reservePanel} from './geometry.js';
function equal(actual, expected) {
    if (JSON.stringify(actual) !== JSON.stringify(expected))
        throw Error(`${JSON.stringify(actual)} != ${JSON.stringify(expected)}`);
}
equal(panelBounds([850, 850, 20, 20], [0, 0, 1440, 843]), [665, 265, 390, 570]);
equal(panelBounds([710, 5, 20, 20], [0, 30, 1440, 870]), [525, 38, 390, 570]);
equal(panelBounds([-700, 1050, 20, 20], [-1920, 0, 1920, 1040]), [-885, 462, 390, 570]);
equal(panelBounds([120, 485, 20, 20], [0, 0, 320, 480]), [8, 8, 304, 464]);
const safe = reservePanel([0, 0, 1920, 1080], [300, 1008, 1320, 72], [0, 0, 1920, 1080]);
equal(safe, [0, 0, 1920, 1008]);
equal(reservePanel(safe, [300, 1008, 1320, 72], [0, 0, 1920, 1080]), safe);
equal(panelBounds([870, 1030, 24, 24], safe), [687, 430, 390, 570]);
equal(panelBounds([690, 450, 24, 24], [0, 0, 800, 400]), [402, 8, 390, 384]);
equal(reservePanel([-1920, -1080, 1920, 1080], [-1920, -1080, 1920, 64], [-1920, -1080, 1920, 1080]), [-1920, -1016, 1920, 1016]);
equal(reservePanel([0, 0, 1920, 1080], [0, 100, 80, 800], [0, 0, 1920, 1080]), [80, 0, 1840, 1080]);
equal(panelBounds([30, 390, 20, 20], [80, 0, 1840, 1080]), [88, 115, 390, 570]);
equal(panelBounds([0, 0, 1, 1], [0, 0, 1, 1]), [0, 0, 1, 1]);
const legacy = {_icon: {wm_class: 'twodrive-development', pid: 42}};
equal(ownIndicator(legacy, 42), legacy._icon);
equal(ownIndicator(legacy, 43), null);
equal(ownIndicator({_icon: {wm_class: 'other', pid: 42}}, 42), null);
const sni = {_indicator: {id: 'twodrive-development', busName: 'org.kde.StatusNotifierItem-42-1'}};
equal(ownIndicator(sni, 42), sni);
equal(ownIndicator(sni, 43), null);
print('TwoDrive host geometry/identity checks passed');
