/*
 * The part of FreeBSD's sound core (sys/dev/sound/pcm) that the HD Audio driver (hdac, hdacc, hdaa) calls, for the
 * FreeBSD-on-xnu layer. FreeBSD's pcm core turns a channel into /dev/dsp with feeders, vchans and an OSS mixer; on macOS
 * the channel is an IOAudioEngine instead (nmx_audio_engine.cpp), so this header keeps the driver-facing API - channel
 * and mixer classes, the DMA buffer, pcm_register/pcm_addchan, mix_* - and nmx_pcm.c implements it. Constants and
 * structures are FreeBSD's (sys/dev/sound/pcm/sound.h, channel.h, mixer.h), so the driver compiles unchanged.
 */
#ifndef NMX_SND_SOUND_H
#define NMX_SND_SOUND_H

#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/bus.h>
#include <sys/rman.h>
#include <sys/sysctl.h>
#include <sys/taskqueue.h>
#include <machine/bus.h>
#include <sys/soundcard.h>
#include <dev/sound/pcm/matrix.h>
#include <sys/interrupt.h>
#include <machine/resource.h>
#include "nmx_kobj.h"

#define SND_STATUSLEN		64

#define SD_F_SIMPLEX		0x00000001
#define SD_F_SOFTPCMVOL		0x00000004
#define SD_F_BUSY		0x00000008
#define SD_F_MPSAFE		0x00000010
#define SD_F_REGISTERED		0x00000020
#define SD_F_BITPERFECT		0x00000040
#define SD_F_EQ			0x00000200

#define AFMT_CONVERTIBLE	(AFMT_8BIT | AFMT_16BIT | AFMT_24BIT | AFMT_32BIT)
#define AFMT_PASSTHROUGH		AFMT_AC3
#define AFMT_PASSTHROUGH_RATE		48000
#define AFMT_PASSTHROUGH_CHANNEL	2
#define AFMT_PASSTHROUGH_EXTCHANNEL	0
#define AFMT_ENCODING_MASK	0xf00fffff
#define AFMT_CHANNEL_MASK	0x07f00000
#define AFMT_CHANNEL_SHIFT	20
#define AFMT_CHANNEL_MAX	0x7f
#define AFMT_EXTCHANNEL_MASK	0x08000000
#define AFMT_EXTCHANNEL_SHIFT	27
#define AFMT_EXTCHANNEL_MAX	1
#define AFMT_ENCODING(v)	((v) & AFMT_ENCODING_MASK)
#define AFMT_EXTCHANNEL(v)	(((v) & AFMT_EXTCHANNEL_MASK) >> AFMT_EXTCHANNEL_SHIFT)
#define AFMT_CHANNEL(v)		(((v) & AFMT_CHANNEL_MASK) >> AFMT_CHANNEL_SHIFT)
#define AFMT_BIT(v)		(((v) & AFMT_32BIT) ? 32 : (((v) & AFMT_24BIT) ? 24 : \
				((((v) & AFMT_16BIT) || ((v) & AFMT_PASSTHROUGH)) ? 16 : 8)))
#define AFMT_BPS(v)		(AFMT_BIT(v) >> 3)
#define AFMT_ALIGN(v)		(AFMT_BPS(v) * AFMT_CHANNEL(v))
#define SND_FORMAT(f, c, e)	(AFMT_ENCODING(f) | (((c) << AFMT_CHANNEL_SHIFT) & AFMT_CHANNEL_MASK) | \
				(((e) << AFMT_EXTCHANNEL_SHIFT) & AFMT_EXTCHANNEL_MASK))

enum { PCMDIR_PLAY = 1, PCMDIR_PLAY_VIRTUAL, PCMDIR_REC, PCMDIR_REC_VIRTUAL };
#define PCMTRIG_START	1
#define PCMTRIG_EMLDMAWR 2
#define PCMTRIG_EMLDMARD 3
#define PCMTRIG_STOP	0
#define PCMTRIG_ABORT	-1
#define PCMTRIG_COMMON(x) ((x) == PCMTRIG_START || (x) == PCMTRIG_STOP || (x) == PCMTRIG_ABORT)

struct pcmchan_caps {
	uint32_t minspeed, maxspeed;
	uint32_t *fmtlist;
	uint32_t caps;
};

struct pcmchan_matrix {
	int id;
	uint8_t channels, ext;
	struct {
		int type;
		uint32_t members;
	} map[SND_CHN_T_MAX + 1];
	uint32_t mask;
	int8_t offset[SND_CHN_T_MAX];
};

/* the channel's DMA ring. hdaa reads align, maxsize, blksz, blkcnt and buf_addr directly, so those keep FreeBSD's
 * names (sys/dev/sound/pcm/buffer.h); the rest is the port's */
struct snd_dbuf {
	uint8_t *buf;			/* kernel address of the ring */
	unsigned int bufsize;		/* bytes in use: blksz * blkcnt */
	unsigned int maxsize;		/* bytes allocated */
	unsigned int align;
	unsigned int blksz, blkcnt;
	bus_addr_t buf_addr;		/* bus address the controller's BDL points at */
	bus_dma_tag_t dmatag;
	bus_dmamap_t dmamap;
	struct pcm_channel *channel;
};
struct pcm_channel;		/* one play or record channel (nmx_pcm.c) */
/* a PCM device's mixer: a kobj first (the generated MIXER_* callers read ->ops), then the port's state */
struct snd_mixer {
	KOBJ_FIELDS;
	void *devinfo;
	device_t dev;
	uint32_t devs, recdevs, recsrc;
	uint32_t realdev[SOUND_MIXER_NRDEVICES];
	uint32_t parent[SOUND_MIXER_NRDEVICES], child[SOUND_MIXER_NRDEVICES];
	uint8_t level[SOUND_MIXER_NRDEVICES][2];	/* left, right, 0..100 */
};
/* a PCM device: the softc of hdaa's "pcm" children. hdaa adds sysctls under the play/rec nodes; the rest is the
 * port's - the channels pcm_addchan made and the mixer, which the macOS audio engine (nmx_audio_engine.cpp) uses */
#define NMX_PCM_MAXCHAN 8
struct snddev_info {
	device_t dev;
	void *devinfo;
	uint32_t flags;
	char status[SND_STATUSLEN];
	int registered;
	int nchan;
	struct pcm_channel *chan[NMX_PCM_MAXCHAN];
	struct snd_mixer *mixer;
	struct sysctl_ctx_list play_sysctl_ctx, rec_sysctl_ctx;
	struct sysctl_oid *play_sysctl_tree, *rec_sysctl_tree;
	void *engine;			/* the IOAudioEngine for this device, once published */
};

#define CHANNEL_DECLARE(name) static DEFINE_CLASS(name, name ## _methods, sizeof(struct kobj))
#define MIXER_DECLARE(name) static DEFINE_CLASS(name, name ## _methods, sizeof(struct kobj))

#define PCM_SOFTC_SIZE (sizeof(struct snddev_info))

extern int snd_unit;

/* PCM devices and channels */
int pcm_register(device_t dev, char *str);
int pcm_unregister(device_t dev);
void pcm_init(device_t dev, void *devinfo);
int pcm_addchan(device_t dev, int dir, kobj_class_t cls, void *devinfo);
unsigned int pcm_getbuffersize(device_t dev, unsigned int minbufsz, unsigned int deflt, unsigned int maxbufsz);
uint32_t pcm_getflags(device_t dev);
void pcm_setflags(device_t dev, uint32_t val);
void chn_intr(struct pcm_channel *c);
int snd_setup_intr(device_t dev, struct resource *res, int flags, driver_intr_t hand, void *param, void **cookiep);

/* DMA buffers */
int sndbuf_alloc(struct snd_dbuf *b, bus_dma_tag_t dmatag, int dmaflags, unsigned int size);
int sndbuf_resize(struct snd_dbuf *b, unsigned int blkcnt, unsigned int blksz);
bus_addr_t sndbuf_getbufaddr(struct snd_dbuf *b);
void *sndbuf_getbuf(struct snd_dbuf *b);
unsigned int sndbuf_getsize(struct snd_dbuf *b);
unsigned int sndbuf_getblksz(struct snd_dbuf *b);
unsigned int sndbuf_getblkcnt(struct snd_dbuf *b);

/* mixer */
int mixer_init(device_t dev, kobj_class_t cls, void *devinfo);
int mixer_reinit(device_t dev);
void *mix_getdevinfo(struct snd_mixer *m);
void mix_setdevs(struct snd_mixer *m, uint32_t v);
void mix_setrecdevs(struct snd_mixer *m, uint32_t v);
uint32_t mix_getdevs(struct snd_mixer *m);
uint32_t mix_getrecdevs(struct snd_mixer *m);
void mix_setparentchild(struct snd_mixer *m, uint32_t parent, uint32_t childs);
void mix_setrealdev(struct snd_mixer *m, uint32_t dev, uint32_t realdev);
int mix_set(struct snd_mixer *m, unsigned int dev, unsigned int left, unsigned int right);
int mix_setrecsrc(struct snd_mixer *m, uint32_t src);

/* used by channel_if's default getmatrix */
uint32_t feeder_matrix_default_format(uint32_t format);
struct pcmchan_matrix *feeder_matrix_format_map(uint32_t format);

#endif
