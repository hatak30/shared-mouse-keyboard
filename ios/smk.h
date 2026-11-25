#ifndef SMK_H
#define SMK_H
#include <stdint.h>

void smk_set_callback(void (*callback)(void *), void *context);
void smk_set_screen(uint32_t width, uint32_t height);
char *smk_start(const char *config_dir, const char *download_dir);
char *smk_state(void);
char *smk_command(const char *name, const char *a, const char *b);
void smk_input(int32_t kind, double a, double b);
void smk_free_string(char *s);

#endif
