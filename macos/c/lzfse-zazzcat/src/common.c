// Status descriptions.
#include "common.h"

const char *lzfse_zazzcat_status_string(lzfse_zazzcat_status status) {
    switch (status) {
    case LZFSE_ZAZZCAT_OK:
        return "ok";
    case LZFSE_ZAZZCAT_TRUNCATED:
        return "truncated input";
    case LZFSE_ZAZZCAT_CORRUPT:
        return "corrupt input";
    case LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL:
        return "output buffer too small";
    case LZFSE_ZAZZCAT_OUT_OF_MEMORY:
        return "out of memory";
    case LZFSE_ZAZZCAT_INVALID_ARGUMENT:
        return "invalid argument";
    case LZFSE_ZAZZCAT_UNSUPPORTED:
        return "not supported on this system";
    case LZFSE_ZAZZCAT_SYSTEM_ERROR:
        return "system call failed";
    case LZFSE_ZAZZCAT_NOT_APPLIED:
        return "the kernel did not take the compressed form";
    }
    return "unknown status";
}
