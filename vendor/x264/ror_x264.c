/* SPDX-License-Identifier: GPL-2.0-or-later. Built into ror_x264.lib with x264, which is under the same
 * license (THIRD_PARTY_NOTICES.md). */

/* ror's entry points into x264 (crates/ror/src/x264.rs declares them): an encoder set up as ffmpeg 7.1's
 * libx264 wrapper sets one up for `ffmpeg -f rawvideo -pix_fmt nv12 ... -c:v libx264 -preset P -crf C
 * -threads T -f mp4` (libavcodec/libx264.c X264_init), NV12 frames in, each frame's NAL units out with
 * 4-byte lengths, as an MP4 stores them. Built into ror_x264.lib with x264 itself (vendor/x264/build.py). */

#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "x264.h"

/* common/win32thread.c as build.py patches it: the priority each thread x264 starts runs at */
extern int x264_win32_thread_priority;

typedef struct ror_x264
{
    x264_t *encoder;
    /* the first error x264 logged */
    char error[512];
} ror_x264;

static void log_error( void *target, int level, const char *format, va_list arguments )
{
    char *error = target;
    if( level <= X264_LOG_ERROR && !error[0] )
        vsnprintf( error, 512, format, arguments );
}

/* The priority of the threads every encoder opened after this starts (Windows' THREAD_PRIORITY_*). */
void ror_x264_thread_priority( int priority )
{
    x264_win32_thread_priority = priority;
}

/* An encoder of width x height NV12 frames: `preset` (x264_param_default_preset), CRF `crf`, `threads`
 * frame threads, frames at fps_num / fps_den a second with timestamps in timebase_num / timebase_den
 * seconds, BT.709 primaries and matrix, the sRGB transfer, limited range, chroma sited at the centre
 * (VUI chroma_sample_loc_type 1), headers kept out of the frames (ror_x264_headers) and NAL units with
 * 4-byte lengths. NULL with the reason in `error` when x264 refuses. */
ror_x264 *ror_x264_open( int width, int height, int fps_num, int fps_den, int timebase_num, int timebase_den,
                         const char *preset, float crf, int threads, char *error, int error_size )
{
    x264_param_t param;
    ror_x264 *handle = calloc( 1, sizeof(*handle) );
    if( !handle )
    {
        snprintf( error, error_size, "out of memory" );
        return NULL;
    }
    /* in libx264.c's order: the defaults and the preset, then the fields it sets from the codec context */
    if( x264_param_default_preset( &param, preset, NULL ) < 0 )
    {
        snprintf( error, error_size, "x264 has no preset %s", preset );
        free( handle );
        return NULL;
    }
    param.pf_log = log_error;
    param.p_log_private = handle->error;
    param.i_log_level = X264_LOG_ERROR;
    param.i_csp = X264_CSP_NV12;
    param.i_bitdepth = 8;
    param.rc.i_vbv_buffer_size = 0;
    param.rc.i_vbv_max_bitrate = 0;
    param.rc.b_stat_write = 0;
    param.rc.i_rc_method = X264_RC_CRF;
    param.rc.f_rf_constant = crf;
    param.i_width = width;
    param.i_height = height;
    /* av_reduce(0 / 1): no aspect ratio is written for 0 */
    param.vui.i_sar_width = 0;
    param.vui.i_sar_height = 1;
    param.i_timebase_num = timebase_num;
    param.i_timebase_den = timebase_den;
    param.i_fps_num = fps_num;
    param.i_fps_den = fps_den;
    param.analyse.b_psnr = 0;
    param.i_threads = threads;
    param.b_interlaced = 0;
    param.b_open_gop = 0;
    param.i_slice_count = 0;
    param.vui.b_fullrange = 0;
    param.vui.i_colmatrix = 1;  /* AVCOL_SPC_BT709 */
    param.vui.i_colorprim = 1;  /* AVCOL_PRI_BT709 */
    param.vui.i_transfer = 13;  /* AVCOL_TRC_IEC61966_2_1 */
    param.vui.i_chroma_loc = 1; /* AVCHROMA_LOC_CENTER - 1 */
    param.b_repeat_headers = 0;
    param.analyse.b_mb_info = 0;
    /* ffmpeg leaves Annex B start codes and its MP4 muxer swaps each for the unit's 4-byte length: the
     * same bytes as x264 writes without them */
    param.b_annexb = 0;
    handle->encoder = x264_encoder_open( &param );
    if( !handle->encoder )
    {
        snprintf( error, error_size, "x264_encoder_open failed: %s", handle->error[0] ? handle->error : "(no reason logged)" );
        free( handle );
        return NULL;
    }
    return handle;
}

/* The stream's SPS, PPS and x264's version SEI, each with its 4-byte length. 0, or -1 when x264 fails. */
int ror_x264_headers( ror_x264 *handle, const uint8_t **sps, int *sps_size, const uint8_t **pps, int *pps_size,
                      const uint8_t **sei, int *sei_size )
{
    x264_nal_t *nals;
    int count;
    *sps = *pps = *sei = NULL;
    *sps_size = *pps_size = *sei_size = 0;
    if( x264_encoder_headers( handle->encoder, &nals, &count ) < 0 )
        return -1;
    for( int i = 0; i < count; i++ )
    {
        switch( nals[i].i_type )
        {
            case NAL_SPS: *sps = nals[i].p_payload; *sps_size = nals[i].i_payload; break;
            case NAL_PPS: *pps = nals[i].p_payload; *pps_size = nals[i].i_payload; break;
            case NAL_SEI: *sei = nals[i].p_payload; *sei_size = nals[i].i_payload; break;
            default: break;
        }
    }
    return 0;
}

/* Encodes the frame of `luma` and `chroma` (interleaved U and V) at `pts`, or with `luma` NULL takes one
 * of the frames x264 still holds. Returns the bytes of the frame it put out (its NAL units, one after
 * another in x264's memory until the next call), 0 for none yet, -1 when x264 fails. */
int ror_x264_encode( ror_x264 *handle, const uint8_t *luma, int luma_stride, const uint8_t *chroma, int chroma_stride,
                     int64_t pts, const uint8_t **out, int64_t *out_pts, int64_t *out_dts, int *keyframe )
{
    x264_picture_t input, output;
    x264_nal_t *nals;
    int count;
    int size;
    if( luma )
    {
        x264_picture_init( &input );
        input.img.i_csp = X264_CSP_NV12;
        input.img.i_plane = 2;
        input.img.plane[0] = (uint8_t *)luma;
        input.img.i_stride[0] = luma_stride;
        input.img.plane[1] = (uint8_t *)chroma;
        input.img.i_stride[1] = chroma_stride;
        input.i_pts = pts;
    }
    size = x264_encoder_encode( handle->encoder, &nals, &count, luma ? &input : NULL, &output );
    if( size < 0 )
        return -1;
    if( size == 0 || count == 0 )
        return 0;
    *out = nals[0].p_payload;
    *out_pts = output.i_pts;
    *out_dts = output.i_dts;
    *keyframe = output.b_keyframe;
    return size;
}

/* Frames x264 holds that ror_x264_encode with no input still has to put out */
int ror_x264_delayed( ror_x264 *handle )
{
    return x264_encoder_delayed_frames( handle->encoder );
}

/* The first error x264 logged, "" for none */
const char *ror_x264_error( ror_x264 *handle )
{
    return handle->error;
}

void ror_x264_close( ror_x264 *handle )
{
    if( !handle )
        return;
    x264_encoder_close( handle->encoder );
    free( handle );
}
