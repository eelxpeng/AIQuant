from datetime import datetime, timezone
from pathlib import Path
from dataclasses import dataclass
from monitoring.logger import StructuredLogger


HEARTBEAT_FILE = Path("logs/heart-beat.txt")
KILL_SWITCH_FILE = Path("KILL_SWITCH")

@dataclass
class DailyLossLimit:
    max_daily_loss_pct: float = 0.03
    starting_capital: float = 0.0
    _halted: bool = False

    def check(self, current_capital: float, logger: StructuredLogger) -> bool:
        if self._halted: 
            return False
    
        if self.starting_capital <= 0:
            return True
        daily_loss_pct = (self.starting_capital - current_capital) / self.starting_capital
        if daily_loss_pct >= self.max_daily_loss_pct:
            self._halted = True
            logger.error("daily_loss_limit_breached", loss_pct=daily_loss_pct, limit_pct=self.max_daily_loss_pct)
            return False
        
        return True


def send_heartbeat():
    HEARTBEAT_FILE.parent.mkdir(parents=True, exist_ok=True)
    HEARTBEAT_FILE.write_text(datetime.now(timezone.utc).isoformat())

def heartbeat_is_stale(max_age_seconds: int = 120) -> bool:
    if not HEARTBEAT_FILE.exists():
        return True
    last_beat = datetime.fromisoformat(HEARTBEAT_FILE.read_text())
    return (datetime.now(timezone.utc) - last_beat).total_seconds() > max_age_seconds

def check_kill_switch() -> bool:
    return not KILL_SWITCH_FILE.exists()