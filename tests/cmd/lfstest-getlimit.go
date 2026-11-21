//go:build testtools
// +build testtools

package main

import (
	"bufio"
	"fmt"
	"os"
	"strings"

	"github.com/git-lfs/pktline"
)

// Local change: upstream imports these from its internal `lfs` and `tools`
// packages, which this standalone module can't. Both are 1024 in v3.8.1.
const (
	blobSizeCutoff    = 1024 // lfs.BlobSizeCutoff
	memoryBufferLimit = 1024 // tools.MemoryBufferLimit
)

func main() {
	if len(os.Args) == 2 {
		switch os.Args[1] {
		case "--max-bufio-scan-token-size":
			fmt.Print(bufio.MaxScanTokenSize)
		case "--max-pktline-len":
			fmt.Print(pktline.MaxPacketLength)
		case "--max-pointer-size":
			fmt.Print(blobSizeCutoff)
		case "--max-spool-mem-buffer-size":
			fmt.Print(memoryBufferLimit)
		default:
			fmt.Fprintf(os.Stderr, "unknown argument: %s\n", os.Args[1])
			os.Exit(1)
		}
	} else {
		fmt.Fprintf(os.Stderr, "invalid arguments: %s\n", strings.Join(os.Args, " "))
		os.Exit(1)
	}

	os.Exit(0)
}
