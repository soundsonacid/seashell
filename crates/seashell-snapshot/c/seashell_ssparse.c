/* A flat C interface over fd_ssparse, so that the Rust side does not have to mirror the layout of
   fd_ssparse_t or of the result union. */

#include "discof/restore/utils/fd_ssparse.h"

#include <stdlib.h>

/* Must match `Event` in src/lib.rs. */
struct seashell_ssparse_event {
  ulong         bytes_consumed;
  /* FD_SSPARSE_ADVANCE_ACCOUNT_HEADER */
  ulong         slot;
  ulong         data_len;
  ulong         lamports;
  ulong         rent_epoch;
  uchar const * pubkey; /* 32 bytes, valid until the next advance */
  uchar const * owner;  /* 32 bytes, valid until the next advance */
  int           executable;
  /* FD_SSPARSE_ADVANCE_ACCOUNT_DATA: a piece of the current account's data, inside the input */
  uchar const * data;
  ulong         data_sz;
};

fd_ssparse_t *
seashell_ssparse_new( void ) {
  fd_ssparse_t * ssparse = malloc( sizeof(fd_ssparse_t) );
  if( !ssparse ) return NULL;
  return fd_ssparse_init( ssparse );
}

void
seashell_ssparse_delete( fd_ssparse_t * ssparse ) {
  free( ssparse );
}

/* Returns the FD_SSPARSE_ADVANCE_* code.  Every append vec is parsed. */
int
seashell_ssparse_advance( fd_ssparse_t *                  ssparse,
                          uchar const *                   data,
                          ulong                           data_sz,
                          struct seashell_ssparse_event * event ) {
  fd_ssparse_advance_result_t result[1];
  int kind = fd_ssparse_advance( ssparse, data, data_sz, result );
  event->bytes_consumed = result->bytes_consumed;
  switch( kind ) {
    case FD_SSPARSE_ADVANCE_APPENDVEC:
      fd_ssparse_appendvec_parse( ssparse );
      break;
    case FD_SSPARSE_ADVANCE_ACCOUNT_HEADER:
      event->slot       = result->account_header.slot;
      event->data_len   = result->account_header.data_len;
      event->lamports   = result->account_header.lamports;
      event->rent_epoch = result->account_header.rent_epoch;
      event->pubkey     = result->account_header.pubkey;
      event->owner      = result->account_header.owner;
      event->executable = result->account_header.executable;
      break;
    case FD_SSPARSE_ADVANCE_ACCOUNT_DATA:
      event->data    = result->account_data.data;
      event->data_sz = result->account_data.data_sz;
      break;
    default:
      break;
  }
  return kind;
}
