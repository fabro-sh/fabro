#include <string.h>

static char *g_argv_start = 0;
static unsigned long g_argv_len = 0;

__attribute__((constructor))
static void capture_argv(int argc, char **argv, char **envp) {
    (void)envp;
    if (argc <= 0 || !argv || !argv[0]) return;
    g_argv_start = argv[0];
    char *end = argv[argc - 1] + strlen(argv[argc - 1]) + 1;
    g_argv_len = (unsigned long)(end - argv[0]);
}

char *fabro_proctitle_argv_start(void) { return g_argv_start; }
unsigned long fabro_proctitle_argv_len(void) { return g_argv_len; }
