# Builds a 2-input 2-output ML Program package used to pin down argument table index order: y0 = a - b, y1 = a * 2.
import coremltools as ct
from coremltools.converters.mil import Builder as mb
@mb.program(input_specs=[mb.TensorSpec(shape=(1, 4)), mb.TensorSpec(shape=(1, 4))], opset_version=ct.target.iOS18)
def prog(a, b):
    y0 = mb.sub(x=a, y=b, name="y0")
    y1 = mb.mul(x=a, y=mb.const(val=2.0), name="y1")
    return y0, y1
m = ct.convert(prog, convert_to="mlprogram", minimum_deployment_target=ct.target.iOS18, compute_precision=ct.precision.FLOAT32)
m.save("io2.mlpackage")
