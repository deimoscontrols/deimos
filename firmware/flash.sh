#!/bin/sh

cd ./analog_i_rev3 ;

if (cargo flash --chip STM32H743ZITx --probe $1 --release) then
    cd ..
else
    echo "Connection failed - connecting under reset";
    cargo flash --connect-under-reset --chip STM32H743ZITx --probe $1 --release;
    cd ..
fi