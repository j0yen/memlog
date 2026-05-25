KDIR ?= /lib/modules/$(shell uname -r)/build

# Fall back to the newest installed kernel if the running one has no headers.
ifeq ($(wildcard $(KDIR)/Makefile),)
KDIR := /lib/modules/$(shell ls -1 /lib/modules | sort -V | tail -1)/build
endif

PWD := $(shell pwd)

.PHONY: all clean modules_install

all:
	@echo "Building memlog against $(KDIR)"
	$(MAKE) -C $(KDIR) M=$(PWD) modules

clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean

modules_install:
	$(MAKE) -C $(KDIR) M=$(PWD) modules_install
