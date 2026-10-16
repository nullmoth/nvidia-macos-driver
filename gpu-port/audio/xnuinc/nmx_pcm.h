/*
 * The port's sound core, between FreeBSD's HD Audio driver (below, through the channel and mixer classes) and the
 * macOS audio engine (above, nmx_audio_engine.cpp). C, callable from the C++ engine.
 */
#ifndef NMX_PCM_H
#define NMX_PCM_H
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

struct snddev_info;
struct pcm_channel;

#define NMX_DIR_PLAY 1
#define NMX_DIR_REC  3

/* one play or record channel of a PCM device */
int nmx_chan_dir(struct pcm_channel *c);
/* formats the codec accepts (FreeBSD AFMT_* | channel count), 0-terminated; and its rate range */
const uint32_t *nmx_chan_formats(struct pcm_channel *c, uint32_t *minspeed, uint32_t *maxspeed);
int nmx_chan_setformat(struct pcm_channel *c, uint32_t format);
uint32_t nmx_chan_setspeed(struct pcm_channel *c, uint32_t speed);
/* sizes the DMA ring as blkcnt blocks of blksz bytes (an interrupt per block); returns the ring actually used */
int nmx_chan_setfragments(struct pcm_channel *c, uint32_t blksz, uint32_t blkcnt);
void *nmx_chan_ring(struct pcm_channel *c, uint32_t *bytes, uint32_t *blksz);
int nmx_chan_start(struct pcm_channel *c);
int nmx_chan_stop(struct pcm_channel *c);
uint32_t nmx_chan_getptr(struct pcm_channel *c);	/* byte offset in the ring the hardware has reached */
void nmx_chan_set_engine(struct pcm_channel *c, void *engine);

/* PCM device */
int nmx_pcm_nchan(struct snddev_info *sc);
struct pcm_channel *nmx_pcm_chan(struct snddev_info *sc, int i);
const char *nmx_pcm_name(struct snddev_info *sc);	/* "Realtek ALC897 (Analog)" etc. */
/* where the PCM device sits, stable across boots: the controller's PCI address, then the device's position under its
 * codec. Unit numbers are not stable (two controllers attach in parallel), and names repeat (four identical HDMI pins) */
void nmx_pcm_location(struct snddev_info *sc, char *buf, size_t len);
uint32_t nmx_pcm_mixer_devs(struct snddev_info *sc);	/* SOUND_MASK_* the codec has */
int nmx_pcm_mixer_set(struct snddev_info *sc, unsigned dev, unsigned left, unsigned right);	/* 0..100 */

/* the engine side (nmx_audio_engine.cpp) */
void nmx_audio_publish(struct snddev_info *sc);		/* pcm_register: make the IOAudioDevice */
void nmx_audio_unpublish(struct snddev_info *sc);	/* pcm_unregister */
void nmx_audio_block_done(void *engine, struct pcm_channel *c);	/* chn_intr: one DMA block played/recorded */

#ifdef __cplusplus
}
#endif
#endif
