import subprocess
import sys
import time

start = time.time()
result = subprocess.run(
    [
        "bash",
        "scripts/test_brains.sh",
        "cargo",
        "test",
        "--test",
        "daemon_integration_test",
        "test_daemon_spawn_and_health",
        "--",
        "--exact",
        "--ignored",
        "--nocapture",
    ],
    capture_output=True,
    text=True,
    timeout=900,
)
elapsed = time.time() - start
sys.stdout.write(result.stdout[-8000:])
sys.stderr.write(result.stderr[-8000:])
print(f"RC={result.returncode} elapsed={elapsed:.1f}s")
