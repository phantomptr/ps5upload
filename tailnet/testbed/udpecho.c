/* udpecho: echo every UDP datagram back to its sender (test bed only).
 *   udpecho PORT */
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>

int main(int argc, char **argv)
{
	int fd = socket(AF_INET, SOCK_DGRAM, 0);
	struct sockaddr_in sa = {.sin_family = AF_INET, .sin_port = htons((unsigned short)(argc > 1 ? atoi(argv[1]) : 7777))};
	if (fd < 0 || bind(fd, (struct sockaddr *)&sa, sizeof sa) != 0) {
		perror("udpecho");
		return 1;
	}
	for (;;) {
		char buf[2048];
		struct sockaddr_in from;
		socklen_t fl = sizeof from;
		ssize_t n = recvfrom(fd, buf, sizeof buf, 0, (struct sockaddr *)&from, &fl);
		if (n >= 0)
			sendto(fd, buf, (size_t)n, 0, (struct sockaddr *)&from, fl);
	}
}
