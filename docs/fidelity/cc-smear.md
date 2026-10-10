# CC Smear: reach beyond 100 (#658)

Measured locally against Adobe After Effects 2026 on 10 October 2026, using original
procedural images. No Adobe source, artwork, plug-ins or presets were read or copied.
Reference images remain outside version control.

The default Tip Type was measured. Reach accepts −1000…1000 and defaults to 100.
Existing EffectCraft instances keep their saved values and keyframes when the old
−100…100 property range is upgraded.

## Measurements

A 320×160 red/green coordinate ramp, From=(100,80), To=(200,80), Radius=80, at
Reach=50,100,150,200,−100 showed that the brush tip follows `From + reach * (To−From)`.
The brush's rear length is `radius + distance travelled`; its front length and width
retain `radius`. Displacement is the product of smoothstep falloffs along and across
the brush. Both the CPU and GPU implement this measured default-tip behaviour.

Sampling 465 positions per reach (2,325 total), the model's inferred source x differed
from the reference by at most 1.11 pixels, mean 0.29–0.34 pixels per reach. The 8-bit
ramp quantises source x at approximately 1.25 pixels. The committed regression tests
check representative on-axis and off-axis measurements with a 2-pixel tolerance.

A separate transparent 480×240 image containing a circle centred at (160,120),
radius 30, was rendered with From=(160,120), To=(240,120), Radius=60:

| Reach | Reference alpha > 0.5 bounds (right/bottom exclusive) |
|---|---|
| 100 | (161,91)–(257,150) |
| 150 | (160,91)–(294,150) |
| 200 | (153,91)–(333,150) |

Tests assert these bounds within 2 pixels, and the distinctive transparent centre of
the long trail at 200 while the tip and sides remain opaque. This verifies the
reported droplet/trailing behaviour rather than merely verifying a changed image.
GPU regression cases compare against CPU at positive/negative and extreme reaches;
text-layer tests cover the enlarged effect canvas at full and half resolution (#659).

This is a bounded default-tip check, not a claim of full CC Smear parity. Other Tip
Types, exact edge antialiasing and arbitrary orientations against AE are unmeasured.

The canvas grows only for effects that may need it. Expression controls and
colour-only corrections retain the glyph raster; colour effects that can create
alpha (Levels, Levels IC, Curves, Color Link, Colorama and PS Arbitrary Map) retain
canvas expansion. GPU expansion copies into a larger resident texture and does
not read pixels back to the CPU. A small title with Color Control and Tint has a
regression asserting its buffer remains below 1,000 pixels.
