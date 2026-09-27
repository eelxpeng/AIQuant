import json
from datetime import datetime, timezone
from pathlib import Path

LOG_DIR = Path("logs")
LOG_DIR.mkdir(exist_ok=True)

class StructuredLogger:
    def __init__(self, name: str):
        self.name = name
        self.log_file = LOG_DIR / f"{name}.jsonl"
    
    def _write(self, level: str, event: str, **fields):
        entry = {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "level": level,
            "event": event,
            **fields
        }
        with open(self.log_file, "a") as f:
            f.write(json.dumps(entry) + "\n")
        
        print(f"[{level}] {event}: {fields}")

    def info(self, event: str, **fields):
        self._write("INFO", event, **fields)
    
    def warning(self, event: str, **fields):
        self._write("WARNING", event, **fields)
    
    def error(self, event: str, **fields):
        self._write("ERROR", event, **fields)
