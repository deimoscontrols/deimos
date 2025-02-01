#!/bin/sh

cd ./$1 ;

if (cargo flash --chip STM32H743ZITx --probe $2 --release) then
    cd ..
else
    echo "Connection failed - connecting under reset";
    cargo flash --connect-under-reset --chip STM32H743ZITx --probe $2 --release;
    cd ..
fi