#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <objc/runtime.h>
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl3.h>
#include <dlfcn.h>

static NSUInteger (*originalGet)(id,SEL,MTLSamplePosition *,NSUInteger);
static NSUInteger observedGet(id self, SEL sel, MTLSamplePosition *p, NSUInteger n) {
    NSUInteger r=originalGet(self,sel,p,n);
    fprintf(stderr,"SAMPLE_DESCRIPTOR class=%s capacity=%lu result=%lu",object_getClassName(self),(unsigned long)n,(unsigned long)r);
    if(p && n) for(NSUInteger i=0;i<MIN(n,r);i++) fprintf(stderr," [%g,%g]",p[i].x,p[i].y);
    fprintf(stderr,"\n");return r;
}
static NSDictionary *run(NSUInteger samples) {
    GLuint msfb,rb,fb,tex;glGenFramebuffers(1,&msfb);glBindFramebuffer(GL_FRAMEBUFFER,msfb);
    glGenRenderbuffers(1,&rb);glBindRenderbuffer(GL_RENDERBUFFER,rb);glRenderbufferStorageMultisample(GL_RENDERBUFFER,(GLsizei)samples,GL_RGBA8,32,32);
    glFramebufferRenderbuffer(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_RENDERBUFFER,rb);
    GLenum complete=glCheckFramebufferStatus(GL_FRAMEBUFFER);
    glViewport(0,0,32,32);glClearColor(1,0,0,1);glClear(GL_COLOR_BUFFER_BIT);
    const char *vs="#version 410 core\nvoid main(){vec2 p[3]=vec2[3](vec2(-1,-1),vec2(3,-1),vec2(-1,3));gl_Position=vec4(p[gl_VertexID],0,1);}";
    const char *fs="#version 410 core\nout vec4 color;void main(){color=vec4(0,1,0,1);}";
    GLuint v=glCreateShader(GL_VERTEX_SHADER),f=glCreateShader(GL_FRAGMENT_SHADER),prog=glCreateProgram(),vao;
    glShaderSource(v,1,&vs,NULL);glCompileShader(v);glShaderSource(f,1,&fs,NULL);glCompileShader(f);
    glAttachShader(prog,v);glAttachShader(prog,f);glLinkProgram(prog);GLint linked=0;glGetProgramiv(prog,GL_LINK_STATUS,&linked);
    if(!linked){char b[1024];glGetProgramInfoLog(prog,sizeof b,NULL,b);fprintf(stderr,"LINK_ERROR %s\n",b);}
    glGenVertexArrays(1,&vao);glBindVertexArray(vao);glUseProgram(prog);glDrawArrays(GL_TRIANGLES,0,3);
    glGenFramebuffers(1,&fb);glBindFramebuffer(GL_FRAMEBUFFER,fb);glGenTextures(1,&tex);glBindTexture(GL_TEXTURE_2D,tex);
    glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA8,32,32,0,GL_RGBA,GL_UNSIGNED_BYTE,NULL);
    glFramebufferTexture2D(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,tex,0);
    glBindFramebuffer(GL_READ_FRAMEBUFFER,msfb);glBindFramebuffer(GL_DRAW_FRAMEBUFFER,fb);
    glBlitFramebuffer(0,0,32,32,0,0,32,32,GL_COLOR_BUFFER_BIT,GL_NEAREST);
    glBindFramebuffer(GL_READ_FRAMEBUFFER,fb);uint8_t out[4096];memset(out,0x5a,sizeof out);
    glReadPixels(0,0,32,32,GL_RGBA,GL_UNSIGNED_BYTE,out);glFinish();
    NSUInteger bad=0;for(NSUInteger i=0;i<1024;i++) if(out[4*i]!=0 || out[4*i+1]!=255 || out[4*i+2]!=0 || out[4*i+3]!=255)bad++;
    GLenum error=glGetError();glDeleteFramebuffers(1,&msfb);glDeleteFramebuffers(1,&fb);glDeleteRenderbuffers(1,&rb);glDeleteTextures(1,&tex);
    glDeleteVertexArrays(1,&vao);glDeleteProgram(prog);glDeleteShader(v);glDeleteShader(f);
    return @{@"samples":@(samples),@"linked":@(linked),@"framebuffer_status":@(complete),@"gl_error":@(error),@"mismatched_pixels":@(bad),@"first_pixel":@[@(out[0]),@(out[1]),@(out[2]),@(out[3])]};
}
int main(int argc,const char **argv) { @autoreleasepool {
    if(argc>1 && !dlopen(argv[1],RTLD_NOW|RTLD_GLOBAL)) {fprintf(stderr,"dlopen failed: %s\n",dlerror());return 2;}
    id rp=[MTLRenderPassDescriptor renderPassDescriptor];
    Method m=class_getInstanceMethod([rp class],@selector(getSamplePositions:count:));
    originalGet=(void *)method_getImplementation(m);
    class_addMethod([rp class],@selector(getSamplePositions:count:),(IMP)observedGet,method_getTypeEncoding(m));
    class_replaceMethod([rp class],@selector(getSamplePositions:count:),(IMP)observedGet,method_getTypeEncoding(m));
    CGLPixelFormatAttribute attrs[]={kCGLPFAAccelerated,kCGLPFAOpenGLProfile,(CGLPixelFormatAttribute)kCGLOGLPVersion_3_2_Core,(CGLPixelFormatAttribute)0};
    CGLPixelFormatObj pf=NULL;GLint np=0;CGLError e=CGLChoosePixelFormat(attrs,&pf,&np);
    if(e || !pf) {fprintf(stderr,"choose format %d\n",e);return 3;}
    CGLContextObj ctx=NULL;e=CGLCreateContext(pf,NULL,&ctx);CGLDestroyPixelFormat(pf);
    if(e || !ctx) {fprintf(stderr,"create context %d\n",e);return 4;}CGLSetCurrentContext(ctx);
    const char *renderer=(const char *)glGetString(GL_RENDERER), *version=(const char *)glGetString(GL_VERSION);
    GLint max=0;glGetIntegerv(GL_MAX_SAMPLES,&max);NSMutableArray *runs=[NSMutableArray new];
    for(NSNumber *s in @[@4,@8])if(s.intValue<=max)[runs addObject:run(s.unsignedIntegerValue)];
    NSDictionary *r=@{@"renderer":renderer?@(renderer):@"unknown",@"version":version?@(version):@"unknown",@"max_samples":@(max),@"runs":runs};
    NSData *j=[NSJSONSerialization dataWithJSONObject:r options:NSJSONWritingPrettyPrinted error:nil];fwrite(j.bytes,1,j.length,stdout);puts("");
    CGLSetCurrentContext(NULL);CGLDestroyContext(ctx);return 0;
} }
