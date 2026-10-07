viewport: 402x874
mode: Immediate
-----
expect "Count: 0"
click "Increment"
expect "Count: 1"
click "Your name"
type "Ferris"
expect "Ferris"
