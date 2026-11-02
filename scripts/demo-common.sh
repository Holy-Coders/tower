# Shared portable fixture editing for the acceptance demos (BSD and GNU hosts).
edit_fixture_file() {
  python3 - "$1" "$2" "$3" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
before, after = sys.argv[2], sys.argv[3].replace("\\n", "\n")
content = path.read_text()
if before not in content:
    raise SystemExit(f"fixture edit did not match: {path}")
path.write_text(content.replace(before, after))
PY
}
