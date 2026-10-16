"""Read-only release check for fresh get_capabilities(session) snapshots; does not connect or deploy."""
import argparse
import json
from pathlib import Path
import re
import time


def check(snapshots, expected_sha256, expected_commit=None, now_ms=None, max_age_ms=300000):
    if not re.fullmatch(r'[0-9a-fA-F]{64}', expected_sha256):
        raise ValueError('Expected binary SHA-256 must contain 64 hexadecimal characters')
    now_ms = int(time.time()*1000) if now_ms is None else now_ms
    devices = {}
    for snapshot in snapshots:
        value = snapshot.get('structuredContent', snapshot)
        device = value.get('device_id')
        if device not in ('1259251361','1815504540') or device in devices:
            raise ValueError('Snapshots must uniquely identify the primary and backup Windows devices')
        observed = value.get('observed_at_ms')
        if not isinstance(observed,int) or not 0 <= now_ms-observed <= max_age_ms:
            raise ValueError('Capability snapshot is stale or lacks an observation time')
        peer = value.get('peer',{})
        if value.get('negotiated') is not True or peer.get('os') != 'windows' or not value.get('session'):
            raise ValueError('An authenticated negotiated Windows terminal snapshot is required')
        if not peer.get('protocol') or not peer.get('package_version'):
            raise ValueError('Peer protocol and package version must be observed')
        if str(peer.get('executable_sha256','')).lower() != expected_sha256.lower():
            raise ValueError('Peer on-disk executable SHA-256 differs from the release artifact')
        if expected_commit is not None and peer.get('build_commit') != expected_commit:
            raise ValueError('Peer compile-time build commit differs from the expected release')
        devices[device] = {'protocol':peer.get('protocol'),'package_version':peer.get('package_version'),
                           'build_commit':peer.get('build_commit'),'executable_sha256':peer.get('executable_sha256')}
    if set(devices) != {'1259251361','1815504540'} or devices['1259251361'] != devices['1815504540']:
        raise ValueError('Both devices must have identical protocol/version/commit/hash observations')
    return {'devices':devices,'on_disk_release_match':True,'loaded_image_attested':False,
            'limitation':'Snapshots are caller-supplied evidence. Hashes describe current_exe on disk; this is not loaded-image attestation.'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('snapshots',nargs=2,type=Path)
    parser.add_argument('--sha256',required=True)
    parser.add_argument('--commit')
    args = parser.parse_args()
    print(json.dumps(check([json.loads(p.read_text(encoding='utf-8')) for p in args.snapshots],args.sha256,args.commit),indent=2))
