"""Original procedural airsoft AEG report and chipped-plywood impact. No external assets."""
from pathlib import Path
import wave, struct, math, random
from PIL import Image
root=Path(__file__).resolve().parents[1]/'assets'
rng=random.Random(812);rate=44100;count=int(rate*0.28); samples=[]
for i in range(count):
 t=i/rate
 # Spring/mechanism snap followed by a small room reflection tail.
 v=0.46*rng.uniform(-1,1)*math.exp(-t*70)+0.23*math.sin(2*math.pi*(145*t+85*t*t))*math.exp(-t*32)
 if t>0.014:v+=0.17*rng.uniform(-1,1)*math.exp(-(t-0.014)*28)
 samples.append(max(-1,min(1,v)))
with wave.open(str(root/'audio/aeg.wav'),'wb') as f:
 f.setnchannels(1);f.setsampwidth(2);f.setframerate(rate);f.writeframes(b''.join(struct.pack('<h',int(v*32767)) for v in samples))
im=Image.new('RGBA',(128,128));p=im.load()
for y in range(128):
 for x in range(128):
  dx=(x-63.5)/64;dy=(y-63.5)/64;r=math.hypot(dx,dy);a=math.atan2(dy,dx)
  edge=0.43+0.08*math.sin(a*7)+0.05*math.sin(a*13)
  if r<0.18: p[x,y]=(32,24,15,235)
  elif r<edge:
   c=rng.randint(-22,22);p[x,y]=(170+c,121+c,64+c,int(230*(1-r/0.8)))
  else:p[x,y]=(0,0,0,0)
im.save(root/'textures/impact.png')
