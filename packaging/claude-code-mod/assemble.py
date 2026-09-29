#!/usr/bin/env python3
"""Build a self-contained, signed Claude Code plugin and local marketplace."""
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'dist/claude-code-mod'
PLUGIN = OUT / 'marketplace/plugins/z-report'
TARGET = 'aarch64-apple-darwin'


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + '\n')


def main():
    if (platform.system(), platform.machine()) != ('Darwin', 'arm64'):
        raise SystemExit('This package supports macOS Apple Silicon')
    subprocess.run(['cargo', 'build', '--locked', '--release', '-p', 'z-report-cli', '--target', TARGET], cwd=ROOT, check=True)
    if OUT.exists():
        shutil.rmtree(OUT)
    shutil.copytree(ROOT / 'mods/claude-code', PLUGIN,
                    ignore=shutil.ignore_patterns('bin', 'types', 'tests', 'tsconfig.json', 'compatibility.json'))
    (PLUGIN / 'bin').mkdir()
    binary = PLUGIN / 'bin/z-report'
    shutil.copy2(ROOT / f'target/{TARGET}/release/z-report-engine', binary)
    identity = os.environ.get('APPLE_SIGNING_IDENTITY', '-')
    sign = ['codesign', '--force', '--sign', identity]
    if identity != '-':
        sign += ['--options', 'runtime', '--timestamp']
    subprocess.run([*sign, str(binary)], check=True)
    subprocess.run(['codesign', '--verify', '--strict', str(binary)], check=True)
    info = json.loads(run(str(binary), 'info'))['data']
    version = json.loads((PLUGIN / '.claude-plugin/plugin.json').read_text())['version']
    desktop_version = re.search(r'^version = "([^"]+)"', (ROOT / 'src-tauri/Cargo.toml').read_text(), re.M).group(1)
    if version != info['version'] or version != desktop_version:
        raise SystemExit('Plugin, engine and desktop versions must match src-tauri/Cargo.toml')
    write_json(PLUGIN / 'compatibility.json', {
        'protocol': info['protocol'], 'databaseVersion': info['database_version'],
        'testedHost': info['tested_host'], 'target': TARGET,
        'featureFlag': 'CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1',
        'signing': 'ad-hoc' if identity == '-' else 'Developer ID',
        'sourceRevision': run('git', 'rev-parse', 'HEAD'),
        'sourceDirty': bool(run('git', 'status', '--porcelain')),
        'engineVersion': info['version'], 'pluginVersion': version,
    })
    write_json(OUT / 'marketplace/.claude-plugin/marketplace.json', {
        'name': 'z-report', 'owner': {'name': 'Z Report'},
        'plugins': [{'name': 'z-report', 'source': './plugins/z-report', 'version': version,
                     'description': 'An evidence-backed journal of local Claude Code and Codex work'}],
    })
    write_json(PLUGIN / 'checksums.json', {
        str(p.relative_to(PLUGIN)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted(PLUGIN.rglob('*')) if p.is_file()
    })
    archive = Path(shutil.make_archive(str(OUT / f'z-report-{version}-{TARGET}'), 'zip', PLUGIN))
    if identity != '-' and os.environ.get('APPLE_API_KEY_PATH'):
        subprocess.run(['xcrun', 'notarytool', 'submit', str(archive), '--key', os.environ['APPLE_API_KEY_PATH'],
                        '--key-id', os.environ['APPLE_API_KEY'], '--issuer', os.environ['APPLE_API_ISSUER'], '--wait'], check=True)
    (OUT / 'SHA256SUMS').write_text(f'{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n')
    dev = ROOT / 'mods/claude-code'
    if (dev / 'bin').exists():
        shutil.rmtree(dev / 'bin')
    (dev / 'bin').mkdir()
    shutil.copy2(binary, dev / 'bin/z-report')
    shutil.copy2(PLUGIN / 'compatibility.json', dev / 'compatibility.json')
    print(PLUGIN)


if __name__ == '__main__':
    main()
