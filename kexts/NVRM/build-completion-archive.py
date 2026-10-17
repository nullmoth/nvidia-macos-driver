from pathlib import Path
import shlex,subprocess,hashlib,json
import argparse
parser=argparse.ArgumentParser(description="Compile the paired NVIDIA 610 display completion archive without modifying the original archive.")
parser.add_argument('--vendor',type=Path,required=True,help='NVIDIA 610.57.04 src/nvidia-modeset directory with existing Darwin compile commands')
parser.add_argument('--output',type=Path,required=True,help='Empty owned output directory')
a=parser.parse_args();home=Path.home();vendor=a.vendor.resolve();base=a.output.resolve()
if base.exists() and any(base.iterdir()):raise SystemExit('Output directory must be empty')
base.mkdir(parents=True,exist_ok=True)
# Each patched vendor source is copied out, patched in order, compiled with its own recorded compile command and
# replaces its object in a copy of the vendor archive. The vendor tree itself is never modified.
sources=[('kapi/src/nvkms-kapi.c',['610-display-completion.patch','610-setmode-reason.patch']),
         ('src/nvkms-rm.c',['610-backlight-register-log.patch'])]
lines=(vendor/'_out/Darwin_x86_64/compile_cmds.sh').read_text().splitlines()
archive=base/'libnvkms.a';archive.write_bytes((vendor/'_out/Darwin_x86_64/libnvkms.a').read_bytes())
built=[]
for rel,patches in sources:
    stem=Path(rel).stem;src=base/(stem+'.c');obj=base/(stem+'.o')
    src.write_bytes((vendor/rel).read_bytes())
    for name in patches:
        patch=Path(__file__).resolve().parent/'patches'/name
        subprocess.run(['patch','--batch','--forward',str(src),str(patch)],check=True)
    line=next(x for x in lines if ' -c '+rel+' ' in x)
    args=shlex.split(line.split(' && ')[0])
    for flag,val in [('-c',src),('-o',obj),('-MF',base/(stem+'.d')),('-MT',obj)]:args[args.index(flag)+1]=str(val)
    # The recorded commands name neither architecture nor deployment target (recorded on an x86_64 macOS 15 host);
    # the vendor objects are x86_64 with minos 15.0, and the kext links for 15.0.
    args+=['-arch','x86_64','-mmacosx-version-min=15.0','-ffile-prefix-map='+str(home)+'=/build']
    with (base/(stem+'-build.log')).open('w') as log:subprocess.run(args,cwd=vendor,stdout=log,stderr=subprocess.STDOUT,check=True)
    subprocess.run(['ar','r',str(archive),str(obj)],check=True)
    built+=[src,obj]
metadata=base/'g_nvid_string.c'
metadata.write_text('const char NV_KMS_ID[] = "nvidia id: NVIDIA UNIX Open Kernel Module for x86_64 610.57.04 Release Build (NullMoth Systems)";\nconst char *const pNV_KMS_ID = NV_KMS_ID + 11;\n')
line=next(x for x in lines if ' -c _out/Darwin_x86_64/g_nvid_string.c ' in x)
args=shlex.split(line.split(' && ')[0])
for flag,val in [('-c',metadata),('-o',base/'g_nvid_string.o'),('-MF',base/'g_nvid_string.d'),('-MT',base/'g_nvid_string.o')]:args[args.index(flag)+1]=str(val)
args+=['-arch','x86_64','-mmacosx-version-min=15.0','-ffile-prefix-map='+str(home)+'=/build']
with (base/'metadata-build.log').open('w') as log:subprocess.run(args,cwd=vendor,stdout=log,stderr=subprocess.STDOUT,check=True)
subprocess.run(['ar','r',str(archive),str(base/'g_nvid_string.o')],check=True)
subprocess.run(['ranlib',str(archive)],check=True)
proof={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in built+[archive]};(base/'completion-build-proof.json').write_text(json.dumps(proof,indent=2)+'\n');print('Vendor completion source, object and paired archive compiled.')
