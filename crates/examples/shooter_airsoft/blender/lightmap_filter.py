"""Offline OIDN filter for Cycles lightmap radiance, before sRGB encoding."""
import shutil
import subprocess
import tempfile
from pathlib import Path

import numpy as np


def denoise(rgb):
    executable = shutil.which('oidnDenoise')
    if not executable:
        raise RuntimeError('Install Open Image Denoise (oidnDenoise) before baking.')
    height, width, channels = rgb.shape
    assert channels == 3
    with tempfile.TemporaryDirectory(prefix='airsoft-lightmap-') as directory:
        source = Path(directory) / 'noisy.pfm'
        output = Path(directory) / 'filtered.pfm'
        with source.open('wb') as stream:
            stream.write(f'PF\n{width} {height}\n-1.0\n'.encode())
            stream.write(np.ascontiguousarray(rgb, dtype='<f4').tobytes())
        subprocess.run([executable, '--device', 'cpu', '--filter', 'RTLightmap',
                        '--hdr', str(source), '--output', str(output),
                        '--threads', '8', '--maxmem', '1024'], check=True)
        with output.open('rb') as stream:
            assert stream.readline().strip() == b'PF'
            assert tuple(map(int, stream.readline().split())) == (width, height)
            scale = float(stream.readline())
            result = np.frombuffer(stream.read(), dtype='<f4' if scale < 0 else '>f4')
            result = result.reshape(height, width, 3).copy() * abs(scale)
        assert np.isfinite(result).all()
        return np.maximum(result, 0)
