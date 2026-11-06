"""Studio's tone curves as ror draws them (shaders/resolve.wgsl), for the analysis scripts: notes/render.md
"Colour pipeline and tone curve" (the default curve) and "Post effects" (ColorGradingEffect's Retro).

The shown value is √T(x) per channel, x = 2^ExposureCompensation · radiance (the bloom and SunRays
added), clipped at 1 after ColorCorrection.

    tone(x)              T(x), the default curve (above 1 from x 12.7)
    display(x)           255 √T(x), clipped at 255
    radiance(value)      display (0-255) -> x; 255 reads as a lower bound
    GRID, TONED          x from 0 to 80 (fine below 1) and T(x) there, for np.interp
    retro(x), retro_display(x), retro_radiance(value)
                         the same for ColorGradingEffect's Retro (TonemapperPreset 1)
"""

import numpy as np

# Up to x = 2.1313 the rational curve of y = SCALE x, fitted on sun-board's tiles; its linear term
# (the toe) and the input's scale refitted on tone-toe's gray tiles without fog.
A, B, C, D, SCALE = 2.725891, 0.00329, 2.307040, 1.951868, 1.0098
# Above, continuous with it, a shoulder approaching its ceiling as a power of the distance (tone-retro):
# T = CEILING - (CEILING - SHOULDER_START) (1 + SHOULDER_K (x - SHOULDER_X))^-SHOULDER_POWER.
SHOULDER_X, SHOULDER_START, CEILING, SHOULDER_K, SHOULDER_POWER = 2.1313, 0.795199, 1.00999, 0.32012, 2.07968
# Retro: log2 of the shown value at log2 x = -10, -9.5, ..., 4, linear in between and past the ends
# (tone-retro).
RETRO_LOG2_SHOWN = np.array([
	-7.2414, -6.7848, -6.3280, -5.8712, -5.4162, -4.9665, -4.5287, -4.1098, -3.7172, -3.3548,
	-3.0230, -2.7207, -2.4448, -2.1915, -1.9547, -1.7230, -1.4883, -1.2476, -0.9922, -0.7298,
	-0.4651, -0.2097, 0.0362, 0.2533, 0.4418, 0.6035, 0.7312, 0.8572, 0.9833,
])


def tone(x):
	x = np.maximum(np.asarray(x, float), 0)
	y = SCALE * x
	rational = y * (A * y + B) / (y * (C * y + D) + 1)
	shoulder = CEILING - (CEILING - SHOULDER_START) * (1 + SHOULDER_K * np.maximum(x - SHOULDER_X, 0)) ** -SHOULDER_POWER
	return np.where(x > SHOULDER_X, shoulder, rational)


def retro(x):
	t = (np.log2(np.maximum(np.asarray(x, float), 1e-9)) + 10) * 2
	i = np.clip(np.floor(t).astype(int), 0, len(RETRO_LOG2_SHOWN) - 2)
	shown = 2 ** (RETRO_LOG2_SHOWN[i] + (RETRO_LOG2_SHOWN[i + 1] - RETRO_LOG2_SHOWN[i]) * (t - i))
	return shown * shown


GRID = np.concatenate([np.linspace(0, 1, 200001), np.linspace(1, 80, 400001)[1:]])
TONED = tone(GRID)
RETRO_TONED = retro(GRID)


def display(x):
	return np.minimum(np.sqrt(tone(x)), 1) * 255


def retro_display(x):
	return np.minimum(np.sqrt(retro(x)), 1) * 255


def radiance(value):
	return np.interp((np.asarray(value, float) / 255.0) ** 2, TONED, GRID)


def retro_radiance(value):
	return np.interp((np.asarray(value, float) / 255.0) ** 2, RETRO_TONED, GRID)
