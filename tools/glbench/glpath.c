// glpath: which GLEngine path (accelerated vertex arrays, or the immediate-mode primitive buffer) a vertex layout takes.
// Draws <draws> indexed quads per frame from a VBO laid out like one of Minecraft's vertex formats, for <seconds>, so
// `sample` can show glDrawElements*_ACC_GL3Exec or *_IMM_GL3Exec.
//   cc -O2 -o glpath glpath.c -framework OpenGL && ./glpath <layout> [seconds]
// layouts: pos (float3) | block (float3 pos, ubyte4 color, float2 uv0, short2 uv2 as integer, byte3 normal + pad)
//          and block minus one attribute: block-nocolor, block-nouv2, block-nonormal, block-uv2float, block-normal4
#define GL_SILENCE_DEPRECATION
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl3.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static double now_s(void) { static mach_timebase_info_data_t tb; if (!tb.denom) mach_timebase_info(&tb); return (double)mach_absolute_time() * tb.numer / tb.denom / 1e9; }
static GLuint shader(GLenum type, const char *src) {
	GLuint s = glCreateShader(type); glShaderSource(s, 1, &src, NULL); glCompileShader(s);
	GLint ok = 0; glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
	if (!ok) { char log[1024]; glGetShaderInfoLog(s, sizeof log, NULL, log); fprintf(stderr, "shader: %s\n", log); exit(1); }
	return s;
}
int main(int argc, char **argv) {
	const char *layout = argc > 1 ? argv[1] : "block"; double secs = argc > 2 ? atof(argv[2]) : 5;
	int color = 1, uv2 = 1, normal = 1, uv2f = 0, normal4 = 0, pos_only = !strcmp(layout, "pos");
	if (!strcmp(layout, "block-nocolor")) color = 0;
	if (!strcmp(layout, "block-nouv2")) uv2 = 0;
	if (!strcmp(layout, "block-nonormal")) normal = 0;
	if (!strcmp(layout, "block-uv2float")) uv2f = 1;
	if (!strcmp(layout, "block-normal4")) normal4 = 1;
	CGLPixelFormatAttribute attrs[] = { kCGLPFAOpenGLProfile, (CGLPixelFormatAttribute)kCGLOGLPVersion_GL4_Core, kCGLPFAAccelerated, 0 };
	CGLPixelFormatObj pf; GLint npf; CGLContextObj ctx;
	if (CGLChoosePixelFormat(attrs, &pf, &npf) || !pf || CGLCreateContext(pf, NULL, &ctx)) { fprintf(stderr, "no GL 4.1 context\n"); return 1; }
	CGLSetCurrentContext(ctx);
	GLuint fbo, rb; glGenFramebuffers(1, &fbo); glGenRenderbuffers(1, &rb);
	glBindFramebuffer(GL_FRAMEBUFFER, fbo); glBindRenderbuffer(GL_RENDERBUFFER, rb); glRenderbufferStorage(GL_RENDERBUFFER, GL_RGBA8, 640, 480);
	glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, rb); glViewport(0, 0, 640, 480);
	char vs[1024], fs[] = "#version 410 core\nin vec4 c; out vec4 o; void main(){ o = c; }\n";
	snprintf(vs, sizeof vs, "#version 410 core\nlayout(location=0) in vec3 Position;%s%s%s%s\nuniform vec3 off; out vec4 c;\n"
		"void main(){ gl_Position = vec4(Position + off, 1.0); c = vec4(1.0)%s%s%s%s; }\n",
		pos_only ? "" : color ? " layout(location=1) in vec4 Color;" : "",
		pos_only ? "" : " layout(location=2) in vec2 UV0;",
		pos_only || !uv2 ? "" : uv2f ? " layout(location=3) in vec2 UV2;" : " layout(location=3) in ivec2 UV2;",
		pos_only || !normal ? "" : " layout(location=4) in vec3 Normal;",
		pos_only ? "" : color ? " * Color" : "", pos_only ? "" : " * UV0.x",
		pos_only || !uv2 ? "" : " * float(UV2.x)", pos_only || !normal ? "" : " * Normal.x");
	GLuint prog = glCreateProgram(); glAttachShader(prog, shader(GL_VERTEX_SHADER, vs)); glAttachShader(prog, shader(GL_FRAGMENT_SHADER, fs));
	glLinkProgram(prog); glUseProgram(prog); GLint uOff = glGetUniformLocation(prog, "off");
	enum { QUADS = 64, STRIDE = 32 };
	unsigned char *v = calloc(QUADS * 4, STRIDE); unsigned short *ix = malloc(QUADS * 6 * sizeof *ix);
	for (int q = 0; q < QUADS; q++) for (int k = 0; k < 4; k++) {
		float p[3] = { (q % 8) * 0.01f + (k == 1 || k == 2) * 0.008f, (q / 8) * 0.01f + (k >= 2) * 0.008f, 0 };
		memcpy(v + (q * 4 + k) * STRIDE, p, 12); memset(v + (q * 4 + k) * STRIDE + 12, 255, 4);
	}
	for (int q = 0; q < QUADS; q++) { unsigned short b = q * 4, t[6] = { b, b + 1, b + 2, b, b + 2, b + 3 }; memcpy(ix + q * 6, t, sizeof t); }
	GLuint vao, vbo, ibo; glGenVertexArrays(1, &vao); glBindVertexArray(vao);
	glGenBuffers(1, &vbo); glBindBuffer(GL_ARRAY_BUFFER, vbo); glBufferData(GL_ARRAY_BUFFER, QUADS * 4 * STRIDE, v, GL_STATIC_DRAW);
	glGenBuffers(1, &ibo); glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ibo); glBufferData(GL_ELEMENT_ARRAY_BUFFER, QUADS * 6 * sizeof *ix, ix, GL_STATIC_DRAW);
	glVertexAttribPointer(0, 3, GL_FLOAT, GL_FALSE, STRIDE, (void *)0); glEnableVertexAttribArray(0);
	if (!pos_only) {
		if (color) { glVertexAttribPointer(1, 4, GL_UNSIGNED_BYTE, GL_TRUE, STRIDE, (void *)12); glEnableVertexAttribArray(1); }
		glVertexAttribPointer(2, 2, GL_FLOAT, GL_FALSE, STRIDE, (void *)16); glEnableVertexAttribArray(2);
		if (uv2) { if (uv2f) glVertexAttribPointer(3, 2, GL_SHORT, GL_FALSE, STRIDE, (void *)24); else glVertexAttribIPointer(3, 2, GL_SHORT, STRIDE, (void *)24); glEnableVertexAttribArray(3); }
		if (normal) { glVertexAttribPointer(4, normal4 ? 4 : 3, GL_BYTE, GL_TRUE, STRIDE, (void *)28); glEnableVertexAttribArray(4); }
	}
	long frames = 0, draws = 0; double t0 = now_s();
	while (now_s() - t0 < secs) {
		glClear(GL_COLOR_BUFFER_BIT);
		for (int d = 0; d < 2000; d++) { glUniform3f(uOff, (d % 64) * 0.025f - 0.9f, (d / 64 % 32) * 0.05f - 0.9f, 0); glDrawElementsInstancedBaseVertex(GL_TRIANGLES, QUADS * 6, GL_UNSIGNED_SHORT, 0, 1, 0); }
		glFinish(); frames++; draws += 2000;
	}
	double dt = now_s() - t0;
	printf("%-16s %.2f us/draw (%ld draws, glGetError 0x%x)\n", layout, dt * 1e6 / draws, draws, glGetError());
	return 0;
}
