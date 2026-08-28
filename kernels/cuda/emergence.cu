struct Params {
    unsigned int canvas_width;
    unsigned int canvas_height;
    unsigned int target_width;
    unsigned int target_height;
    unsigned int actual_windows;
    unsigned int shape_pixels;
    unsigned int background_pixels;
    unsigned int placement_count;
};

struct Score {
    unsigned int score;
    unsigned int digit;
    unsigned int x;
    unsigned int y;
    unsigned int coverage;
    unsigned int leakage;
    unsigned int covered;
    unsigned int total;
    unsigned int leaked;
    unsigned int background_total;
    unsigned int pad0;
    unsigned int pad1;
};

extern "C" __global__ void emergence(
    const unsigned char* digits,
    const unsigned char* target_mask,
    unsigned int canvas_width,
    unsigned int canvas_height,
    unsigned int target_width,
    unsigned int target_height,
    unsigned int actual_windows,
    unsigned int shape_pixels,
    unsigned int background_pixels,
    unsigned int* score_words
) {
    const unsigned int window_index = blockIdx.x * blockDim.x + threadIdx.x;
    if (window_index >= actual_windows || shape_pixels == 0) {
        return;
    }
    const unsigned int max_y = canvas_height - target_height;
    const unsigned int max_x = canvas_width - target_width;
    unsigned int best_score = 0;
    unsigned int best_digit = 0;
    unsigned int best_x = 0;
    unsigned int best_y = 0;
    unsigned int best_coverage = 0;
    unsigned int best_leakage = 1000000;
    unsigned int best_covered = 0;
    unsigned int best_leaked = 0;
    for (unsigned int y_offset = 0; y_offset <= max_y; ++y_offset) {
        for (unsigned int x_offset = 0; x_offset <= max_x; ++x_offset) {
            unsigned int shape_counts[10] = {};
            unsigned int background_counts[10] = {};
            for (unsigned int target_y = 0; target_y < target_height; ++target_y) {
                for (unsigned int target_x = 0; target_x < target_width; ++target_x) {
                    const unsigned int target_index = target_y * target_width + target_x;
                    const unsigned int canvas_index = window_index
                        + (y_offset + target_y) * canvas_width + x_offset + target_x;
                    const unsigned int digit = digits[canvas_index];
                    if (target_mask[target_index] == 1) {
                        ++shape_counts[digit];
                    } else {
                        ++background_counts[digit];
                    }
                }
            }
            for (unsigned int digit = 0; digit < 10; ++digit) {
                const unsigned int matched = shape_counts[digit];
                const unsigned int leaked = background_counts[digit];
                const unsigned int coverage = matched * 1000000 / shape_pixels;
                const unsigned int leakage = background_pixels == 0
                    ? 0 : leaked * 1000000 / background_pixels;
                const float coverage_value = static_cast<float>(coverage) / 1000000.0F;
                const float leakage_value = static_cast<float>(leakage) / 1000000.0F;
                const float density = coverage_value * coverage_value;
                const float contrast = coverage_value > leakage_value
                    ? (coverage_value - leakage_value) / fmaxf(1.0F - leakage_value, 0.000001F)
                    : 0.0F;
                const float cleanliness = 1.0F - leakage_value;
                const unsigned int score = static_cast<unsigned int>(
                    (0.70F * density + 0.20F * contrast + 0.10F * cleanliness) * 1000000.0F
                );
                if (score > best_score
                    || (score == best_score && coverage > best_coverage)
                    || (score == best_score && coverage == best_coverage && leakage < best_leakage)) {
                    best_score = score;
                    best_digit = digit;
                    best_x = x_offset;
                    best_y = y_offset;
                    best_coverage = coverage;
                    best_leakage = leakage;
                    best_covered = matched;
                    best_leaked = leaked;
                }
            }
        }
    }
    unsigned int* output = score_words + window_index * 12;
    output[0] = best_score;
    output[1] = best_digit;
    output[2] = best_x;
    output[3] = best_y;
    output[4] = best_coverage;
    output[5] = best_leakage;
    output[6] = best_covered;
    output[7] = shape_pixels;
    output[8] = best_leaked;
    output[9] = background_pixels;
    output[10] = 0;
    output[11] = 0;
}
