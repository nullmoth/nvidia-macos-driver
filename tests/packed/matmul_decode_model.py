import sys, struct, math
rows={}; cur=None
for l in open(sys.argv[1]):
    if l.endswith(':\n') and not l.startswith(' '): cur=l.strip()[:-1]; rows[cur]={}; continue
    k,v=l.split(':'); rows[cur][int(k.strip()[1:])]=[float(x) for x in v.split()]
def byte(k,j): return (k*16+j)&255
def fp(bits,ebits,mbits,bias,ieee):
    s=bits>>(ebits+mbits); e=(bits>>mbits)&((1<<ebits)-1); m=bits&((1<<mbits)-1)
    if ieee and e==(1<<ebits)-1: v=math.inf if m==0 else math.nan
    # measured: Apple's e4m3 has no NaN code, 0x7f decodes to 480
    elif e==0: v=m*2.0**(1-bias-mbits)
    else: v=(1+m/2**mbits)*2.0**(e-bias)
    return -v if s else v
def dec(t,k,n):
    if t in('mm_i4','mm_u4','mm_fp4'):
        x=(byte(k,n//2)>>(4*(n%2)))&15
        return {'mm_i4':x-16 if x>7 else x,'mm_u4':x,'mm_fp4':fp(x,2,1,1,False)}[t]
    if t in('mm_i2','mm_u2'):
        x=(byte(k,n//4)>>(2*(n%4)))&3
        return x-4 if (t=='mm_i2' and x>1) else x
    b=byte(k,n)
    if t=='mm_e4m3': return fp(b,4,3,7,False)
    if t=='mm_e5m2': return fp(b,5,2,15,True)
    if t=='mm_i8': return b-256 if b>127 else b
    if t=='mm_u8': return b
    lo=byte(k,2*n); hi=byte(k,2*n+1); return struct.unpack('<f',struct.pack('<I',(hi<<24)|(lo<<16)))[0]
allbad=0
for t,r in rows.items():
    bad=tot=0
    for k,vals in r.items():
        for n,v in enumerate(vals):
            # C(m,n) = sum over k' of A(m,k') B(k',n), A = 1 at k' = k, else 0 (0 * inf and 0 * nan are nan)
            acc=0.0
            for kk in range(32):
                b=dec(t,kk,n); a=1.0 if kk==k else 0.0
                acc+= math.nan if (a==0 and (math.isinf(b) or math.isnan(b))) else a*b
            tot+=1
            if (math.isnan(acc) and math.isnan(v)) or acc==v or abs(acc-v)<=abs(acc)*1e-7: continue
            bad+=1
            if bad<=3: print(' ',t,k,n,'apple',v,'model',acc)
    allbad+=bad; print(f'{t}: {tot} values, {bad} differ')
sys.exit(1 if allbad else 0)
