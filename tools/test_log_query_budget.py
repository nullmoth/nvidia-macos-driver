"""Exercise the production log-query cap against an owned fast and stalled command."""
from pathlib import Path
import os, shlex, subprocess, sys, tempfile, time, unittest
ROOT=Path(__file__).resolve().parents[1]
class QueryBudget(unittest.TestCase):
    def test_partial_output_status_and_timer_cancellation(self):
        source=(ROOT/'app/Resources/nullmoth-setup.sh').read_text()
        helper='lshow() {'+source.split('lshow() {',1)[1].split('# Copy newest files',1)[0]
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);fake=root/'query'
            fake.write_text('#!'+sys.executable+'\nimport os,time\nprint("retained evidence",flush=True)\nif os.environ.get("STALL"):time.sleep(30)\nraise SystemExit(3)\n');fake.chmod(0o755)
            helper=helper.replace('/usr/bin/log show',shlex.quote(str(fake)))
            for stalled in [False,True,False]:
                env=dict(os.environ)
                if stalled:env['STALL']='1'
                start=time.monotonic()
                run=subprocess.run(['/bin/bash','-c',helper+'\nlshow 1 fixture; result=$?; echo STATUS=$result'],env=env,capture_output=True,text=True,timeout=4)
                elapsed=time.monotonic()-start
                self.assertEqual(run.returncode,0)
                self.assertIn('retained evidence',run.stdout)
                if stalled:
                    self.assertIn('log query stopped after 1 s',run.stdout)
                    self.assertIn('STATUS=143',run.stdout)
                    self.assertLess(elapsed,3)
                else:
                    self.assertIn('STATUS=3',run.stdout)
                    self.assertNotIn('stopped after',run.stdout)
                    self.assertLess(elapsed,0.9)
if __name__=='__main__':unittest.main()
