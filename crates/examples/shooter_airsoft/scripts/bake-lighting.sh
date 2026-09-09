#!/usr/bin/env sh
# Save Blender edits first; this rebakes the saved scene without rebuilding geometry.
set -eu
cd -- "$(dirname -- "$0")/.."
blender --background -noaudio --python-exit-code 1 blender/bomb_house.blend \
    --python blender/bake_lighting.py --python blender/export_level.py
python3 scripts/check_level.py
