#!/bin/bash

mkdir -p data

echo "Preparing NYC Citi Bike Dataset"

bike_url="https://s3.amazonaws.com/tripdata/2018-citibike-tripdata.zip"
wget "$bike_url" -P data/
file=`basename "$bike_url"`
unzip data/"$file" -d data/

# urls=("https://s3.amazonaws.com/tripdata/201808-citibike-tripdata.csv.zip" "https://s3.amazonaws.com/tripdata/201809-citibike-tripdata.csv.zip" "https://s3.amazonaws.com/tripdata/201810-citibike-tripdata.csv.zip" "https://s3.amazonaws.com/tripdata/201811-citibike-tripdata.csv.zip" "https://s3.amazonaws.com/tripdata/201812-citibike-tripdata.csv.zip")

# for url in "${urls[@]}"; do
#     file=`basename "$url"`
#     wget "$url" -O data/"$file"
#     unzip data/"$file" -d data/
#     rm data/"$file"
# done

input_files=("data/2018-citibike-tripdata/201808-citibike-tripdata.csv" "data/2018-citibike-tripdata/201809-citibike-tripdata.csv" "data/2018-citibike-tripdata/201810-citibike-tripdata.csv" "data/2018-citibike-tripdata/201811-citibike-tripdata.csv" "data/2018-citibike-tripdata/201812-citibike-tripdata.csv")
output_file="data/citibike-tripdata.csv"

touch data/citibike-tripdata.csv

head -n 1 "${input_files[0]}" > "$output_file"

for input_file in "${input_files[@]}"; do
    tail -n +2 "$input_file" >> "$output_file"
done

echo "Combined citibike trip CSV files into '$output_file'"
