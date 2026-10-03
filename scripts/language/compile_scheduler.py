#!/usr/bin/env python3
"""Executable model of the compile-time job scheduler.

``docs/language/semantics/compile-scheduler.json`` states the rules; this module runs them.  A case
describes symbols and the requirements their signature and body issue.  The model starts one job
per demanded goal, lets jobs suspend on unmet requirements, and runs a real ready queue whose order
is chosen by the caller.  The specification requires that order to be unobservable, so the checker
runs every case under several policies and insists on one outcome.
"""

from __future__ import annotations

import random
from typing import Any, Generator


Goal = tuple[str, int]


class Scheduler:
    def __init__(self, specification: dict[str, Any], case: dict[str, Any], policy: str, seed: int = 0):
        self.states: list[str] = specification["states"]
        self.table = {entry["kind"]: entry for entry in specification["requirements"]}
        self.symbols: dict[str, dict[str, Any]] = case["symbols"]
        self.modules: dict[str, list[str]] = case.get("modules", {})
        self.policy = policy
        self.random = random.Random(seed)
        self.reached: dict[str, int] = {name: 0 for name in self.symbols}
        self.jobs: dict[Goal, Generator[Goal, None, None]] = {}
        self.waiting: dict[Goal, Goal] = {}
        self.failed: dict[Goal, dict[str, Any]] = {}
        self.ready: list[Goal] = []
        self.edges: set[tuple[str, str, str]] = set()

    def index(self, state: str) -> int:
        return self.states.index(state)

    # ------------------------------------------------------------------ jobs

    def requirements(self, symbol: str, state: int) -> list[dict[str, Any]]:
        definition = self.symbols[symbol]
        stages = {"signature": self.index("SignatureReady"), "body": self.index("BodyTyped")}

        def stage(requirement: dict[str, Any]) -> int:
            if "in" in requirement:
                return stages[requirement["in"]]
            return self.index(self.table[requirement["kind"]]["issued_while_reaching"])

        issued = [requirement for requirement in definition.get("requires", []) if stage(requirement) == state]
        if definition.get("signature") == "inferred" and state == stages["signature"]:
            issued += [requirement for requirement in definition.get("requires", []) if stage(requirement) == stages["body"]]
        return issued

    def executed(self, symbol: str, seen: list[str]) -> list[str]:
        """Symbols a compile-time evaluation of ``symbol`` runs, in first-execution order."""
        if symbol in seen:
            return seen
        seen.append(symbol)
        for requirement in self.symbols[symbol].get("requires", []):
            if requirement["kind"] == "calls" and requirement.get("executed", True):
                self.executed(requirement["target"], seen)
        return seen

    def job(self, symbol: str, state: int) -> Generator[Goal, None, None]:
        if state > 1:
            yield (symbol, state - 1)
        for requirement in self.requirements(symbol, state):
            entry = self.table[requirement["kind"]]
            target = requirement["target"]
            if target not in self.symbols:
                self.failed[(symbol, state)] = {"code": "F-DIAG-UNBOUND-NAME", "key": (symbol, state), "goals": [self.spell((symbol, state))]}
                return
            needed = self.index(entry["requires"])
            if requirement["kind"] == "names" and target == symbol:
                needed = self.index("Declared")
            targets = [target]
            if needed == self.index("FunctionCertified"):
                targets = self.executed(target, [])
            for callee in targets:
                self.edges.add((symbol, callee, entry["records"]))
                yield (callee, needed)
        failure = self.symbols[symbol].get("fails")
        if failure and self.index(failure["at"]) == state:
            self.failed[(symbol, state)] = {"code": failure["code"], "key": (symbol, state), "goals": [self.spell((symbol, state))]}
            return
        self.reached[symbol] = max(self.reached[symbol], state)

    # ------------------------------------------------------------------ scheduling

    def demand(self, goal: Goal) -> None:
        if goal[1] <= self.reached[goal[0]] or goal in self.jobs or goal in self.failed:
            return
        self.jobs[goal] = self.job(*goal)
        self.ready.append(goal)

    def pick(self) -> Goal:
        if self.policy == "fifo":
            return self.ready.pop(0)
        if self.policy == "lifo":
            return self.ready.pop()
        if self.policy == "sorted":
            self.ready.sort()
            return self.ready.pop(0)
        return self.ready.pop(self.random.randrange(len(self.ready)))

    def met(self, goal: Goal) -> bool:
        return goal[1] <= self.reached[goal[0]]

    def step(self, goal: Goal) -> None:
        """Run one job until it finishes or waits on an unmet goal."""
        job = self.jobs[goal]
        while True:
            try:
                needed = next(job)
            except StopIteration:
                del self.jobs[goal]
                self.waiting.pop(goal, None)
                for waiter, target in list(self.waiting.items()):
                    if target == goal and (self.met(goal) or goal in self.failed):
                        if self.met(goal):
                            del self.waiting[waiter]
                            self.ready.append(waiter)
                return
            if needed[1] == 0 or self.met(needed):
                continue
            self.waiting[goal] = needed
            self.demand(needed)
            return

    def run(self, roots: list[Goal]) -> dict[str, Any]:
        for goal in roots:
            self.demand(goal)
        while self.ready:
            self.step(self.pick())
        return self.outcome()

    # ------------------------------------------------------------------ outcome

    def spell(self, goal: Goal) -> str:
        return f"{goal[0]}:{self.states[goal[1]]}"

    def outcome(self) -> dict[str, Any]:
        """Quiescence: classify every unmet goal as a cycle member or as waiting on a failure."""
        diagnostics = list(self.failed.values())
        stuck = {goal: target for goal, target in self.waiting.items() if goal in self.jobs}
        in_cycle: set[Goal] = set()
        for start in sorted(stuck):
            if start in in_cycle:
                continue
            path = [start]
            cursor = stuck.get(start)
            while cursor in stuck and cursor not in path:
                path.append(cursor)
                cursor = stuck[cursor]
            if cursor in path:
                cycle = path[path.index(cursor) :]
                if not in_cycle.intersection(cycle):
                    smallest = cycle.index(min(cycle))
                    cycle = cycle[smallest:] + cycle[:smallest]
                    in_cycle.update(cycle)
                    diagnostics.append({"code": "F-DIAG-COMPILE-CYCLE", "key": cycle[0], "goals": [self.spell(goal) for goal in cycle]})
        diagnostics.sort(key=lambda diagnostic: diagnostic["key"])
        diagnostics = [{"code": diagnostic["code"], "goals": diagnostic["goals"]} for diagnostic in diagnostics]
        secondary = [
            {"code": "F-DIAG-DEPENDENCY-FAILED", "goals": [self.spell(goal), self.spell(stuck[goal])]}
            for goal in sorted(stuck) if goal not in in_cycle and stuck[goal][0] != goal[0]
        ]
        return {
            "states": {name: self.states[index] for name, index in sorted(self.reached.items())},
            "diagnostics": diagnostics + secondary,
            "edges": [list(edge) for edge in sorted(self.edges)],
        }


def roots_of(specification: dict[str, Any], case: dict[str, Any]) -> list[Goal]:
    states = specification["states"]
    goals: list[Goal] = []
    for demand in case["demands"]:
        if "module" in demand:
            state = "SignatureReady" if demand["goal"] == "seal" else "FunctionCertified"
            goals += [(symbol, states.index(state)) for symbol in case["modules"][demand["module"]]]
        else:
            goals.append((demand["symbol"], states.index(demand["state"])))
    return goals


def run_case(specification: dict[str, Any], case: dict[str, Any], policy: str = "fifo", seed: int = 0, reverse_roots: bool = False) -> dict[str, Any]:
    roots = roots_of(specification, case)
    if reverse_roots:
        roots.reverse()
    return Scheduler(specification, case, policy, seed).run(roots)


def schedules() -> list[dict[str, Any]]:
    """The scheduling orders every case must agree under."""
    orders = [{"policy": policy, "reverse_roots": reverse} for policy in ("fifo", "lifo", "sorted") for reverse in (False, True)]
    orders += [{"policy": "random", "seed": seed, "reverse_roots": seed % 2 == 1} for seed in range(12)]
    return orders
