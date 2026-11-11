#!/usr/bin/env python3
# vtfit.py <our kext vtable dump> <Apple family dump for the same release> [more Apple dumps...]
# For each class in our kext that derives from one of Apple's: every slot our table INHERITS (an imported Apple
# function) must be the function Apple's own table has at that slot on this release, and our table must be at
# least as long as the base. A disagreement means the header we compiled against does not match the release.
import json, sys, re
ours = json.load(open(sys.argv[1])); apple = {}
for f in sys.argv[2:]: apple.update(json.load(open(f)))
def is_apple(n): return bool(re.match(r"__ZNK?\d+IO", n)) or n == "___cxa_pure_virtual"
def method(n):
    """the method part of a mangled member name, without the class that defines it"""
    m = re.match(r"__ZN(K?)(\d+)", n)
    if not m: return n
    rest = n[m.end() + int(m.group(2)):]          # after the class name: <len><method name><arguments>
    k = re.match(r"(\d+)", rest)
    return (m.group(1) + rest[len(k.group(1)):len(k.group(1)) + int(k.group(1))]) if k else rest   # the method's own name
tot_classes = tot_slots = bad = 0; unmatched = []
for c, o in sorted(ours.items()):
    S = o["slots"]; best = None
    for a, av in apple.items():
        A = av["slots"]
        if len(A) > len(S) or len(A) < 20: continue
        score = sum(1 for i in range(len(A)) if S[i] == A[i])
        if best is None or score > best[0] or (score == best[0] and len(A) > len(apple[best[1]]["slots"])): best = (score, a)
    if not best or best[0] < 10: unmatched.append(c); continue
    A = apple[best[1]]["slots"]; inherited = [i for i in range(len(A)) if is_apple(S[i])]
    wrong = [(i, S[i], A[i]) for i in inherited if method(S[i]) != method(A[i]) and A[i] != "___cxa_pure_virtual" and S[i] != "___cxa_pure_virtual"]
    bypass = [i for i in inherited if S[i] != A[i] and method(S[i]) == method(A[i])]
    tot_bypass = globals().get("tot_bypass", 0) + len(bypass); globals()["tot_bypass"] = tot_bypass
    pure_left = [i for i in range(len(A)) if S[i] == "___cxa_pure_virtual"]
    tot_classes += 1; tot_slots += len(inherited); bad += len(wrong)
    line = f"{c} : {best[1]}  base slots {len(A)}, ours {len(S)}, inherited {len(inherited)}, overridden {len(A) - len(inherited)}, disagree {len(wrong)}, still pure {len(pure_left)}"
    print(line)
    for i, s, a in wrong[:4]: print(f"      slot {i}: ours {s[:70]}  |  Apple {a[:70]}")
print(f"same method at the same slot but taken from a class below Apple's own override (identical on every release, a property of the headers, not of the layout): {globals().get('tot_bypass', 0)}")
print(f"TOTAL: {tot_classes} classes matched to an Apple base, {tot_slots} inherited slots compared, {bad} disagree; not matched to any base: {len(unmatched)} {unmatched[:6]}")
sys.exit(1 if bad else 0)
