/* The four fd_log functions that fd_ssparse.c reaches through FD_LOG_WARNING, FD_LOG_ERR and
   FD_TEST, written to stderr.  This stands in for libfd_util's logger, which would need the whole
   Firedancer boot sequence. */

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

long
fd_log_wallclock( void ) {
  struct timespec now;
  clock_gettime( CLOCK_REALTIME, &now );
  return now.tv_sec * 1000000000L + now.tv_nsec;
}

/* Formats the message.  The buffer is per thread, as in Firedancer. */
char const *
fd_log_private_0( char const * fmt, ... ) {
  static _Thread_local char msg[ 4096 ];
  va_list args;
  va_start( args, fmt );
  vsnprintf( msg, sizeof(msg), fmt, args );
  va_end( args );
  return msg;
}

void
fd_log_private_1( int          level,
                  long         now,
                  char const * file,
                  int          line,
                  char const * func,
                  char const * msg ) {
  (void)now;
  fprintf( stderr, "fd_ssparse[%d] %s(%d) %s: %s\n", level, file, line, func, msg );
}

void
fd_log_private_2( int          level,
                  long         now,
                  char const * file,
                  int          line,
                  char const * func,
                  char const * msg ) {
  fd_log_private_1( level, now, file, line, func, msg );
  abort();
}
