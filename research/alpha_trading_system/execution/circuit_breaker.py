import time
from dataclasses import dataclass

@dataclass
class CircuitBreaker:
    failure_threshold: int = 5
    cooldown_seconds: int = 300
    _consecutive_failures: int = 0
    _open_until: float = 0.0

    def record_success(self):
        self._consecutive_failures = 0
    
    def record_failure(self):
        self._consecutive_failures += 1
        if self._consecutive_failures >= self.failure_threshold:
            self._open_until = time.time() + self.cooldown_seconds
            print(f"CIRCUIT BREAKER OPEN: {self._consecutive_failures} consecutive "
                  f"failures. Halting order submission for {self.cooldown_seconds}s.")
    
    def is_open(self) -> bool:
        return time.time() < self._open_until
