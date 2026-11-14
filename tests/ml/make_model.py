# Builds a fixed 2-layer MLP (4 -> 8 relu -> 3) as an ML Program package.
import numpy as np, coremltools as ct
from coremltools.converters.mil import Builder as mb
rng = np.random.RandomState(7)
W1 = rng.uniform(-1, 1, (8, 4)).astype(np.float32); b1 = rng.uniform(-.5, .5, 8).astype(np.float32)
W2 = rng.uniform(-1, 1, (3, 8)).astype(np.float32); b2 = rng.uniform(-.5, .5, 3).astype(np.float32)
@mb.program(input_specs=[mb.TensorSpec(shape=(1, 4))], opset_version=ct.target.iOS18)
def prog(x):
    h = mb.relu(x=mb.linear(x=x, weight=W1, bias=b1))
    return mb.linear(x=h, weight=W2, bias=b2, name="y")
m = ct.convert(prog, convert_to="mlprogram", minimum_deployment_target=ct.target.iOS18,
               compute_precision=ct.precision.FLOAT32)
m.save("mlp.mlpackage")

x = np.array([[0.5, -1.0, 2.0, 0.25]], np.float32)
print("ref", np.maximum(x @ W1.T + b1, 0) @ W2.T + b2)
open("weights.txt", "w").write(" ".join(" ".join(map(repr, a.ravel().tolist())) for a in (W1, b1, W2, b2)))
