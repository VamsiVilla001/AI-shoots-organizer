#include <opencv2/calib3d.hpp>
#include <opencv2/core.hpp>
#include <opencv2/features2d.hpp>
#include <opencv2/imgproc.hpp>
#include <opencv2/video/tracking.hpp>

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <vector>

struct TeoTrackedBox {
    float x;
    float y;
    float width;
    float height;
    float confidence;
    int valid;
};

namespace {

void write_box(TeoTrackedBox& output,
               float x,
               float y,
               float width,
               float height,
               float confidence,
               int image_width,
               int image_height) {
    const float normalised_x = std::clamp(x / image_width, 0.0f, 1.0f);
    const float normalised_y = std::clamp(y / image_height, 0.0f, 1.0f);
    output = TeoTrackedBox{
        normalised_x,
        normalised_y,
        std::clamp(width / image_width, 0.0f, 1.0f - normalised_x),
        std::clamp(height / image_height, 0.0f, 1.0f - normalised_y),
        std::clamp(confidence, 0.0f, 1.0f),
        1,
    };
}

} // namespace

extern "C" {

int teo_opencv_track_boxes(const unsigned char* previous_rgb,
                           const unsigned char* current_rgb,
                           int width,
                           int height,
                           const float* boxes,
                           std::size_t box_count,
                           TeoTrackedBox* output) noexcept {
    if (previous_rgb == nullptr || current_rgb == nullptr || boxes == nullptr || output == nullptr ||
        width <= 0 || height <= 0) {
        return -1;
    }

    try {
        const cv::Mat previous_rgb_mat(height, width, CV_8UC3, const_cast<unsigned char*>(previous_rgb));
        const cv::Mat current_rgb_mat(height, width, CV_8UC3, const_cast<unsigned char*>(current_rgb));
        cv::Mat previous_gray;
        cv::Mat current_gray;
        cv::cvtColor(previous_rgb_mat, previous_gray, cv::COLOR_RGB2GRAY);
        cv::cvtColor(current_rgb_mat, current_gray, cv::COLOR_RGB2GRAY);

        std::vector<cv::Point2f> previous_points;
        std::vector<std::size_t> owners;
        std::vector<std::size_t> feature_counts(box_count, 0);

        for (std::size_t index = 0; index < box_count; ++index) {
            output[index] = TeoTrackedBox{0, 0, 0, 0, 0, 0};
            const float x = boxes[index * 4] * width;
            const float y = boxes[index * 4 + 1] * height;
            const float w = boxes[index * 4 + 2] * width;
            const float h = boxes[index * 4 + 3] * height;
            const int left = std::clamp(static_cast<int>(std::floor(x)), 0, width - 1);
            const int top = std::clamp(static_cast<int>(std::floor(y)), 0, height - 1);
            const int right = std::clamp(static_cast<int>(std::ceil(x + w)), left + 1, width);
            const int bottom = std::clamp(static_cast<int>(std::ceil(y + h)), top + 1, height);
            if (right - left < 16 || bottom - top < 16) {
                continue;
            }

            std::vector<cv::Point2f> local;
            cv::goodFeaturesToTrack(previous_gray(cv::Rect(left, top, right - left, bottom - top)),
                                    local, 32, 0.01, 3.0);
            feature_counts[index] = local.size();
            for (cv::Point2f point : local) {
                point.x += static_cast<float>(left);
                point.y += static_cast<float>(top);
                previous_points.push_back(point);
                owners.push_back(index);
            }
        }

        if (previous_points.empty()) {
            return 0;
        }

        std::vector<cv::Point2f> current_points;
        std::vector<unsigned char> forward_status;
        std::vector<float> forward_error;
        cv::calcOpticalFlowPyrLK(previous_gray, current_gray, previous_points, current_points,
                                forward_status, forward_error, cv::Size(21, 21), 3,
                                cv::TermCriteria(cv::TermCriteria::COUNT | cv::TermCriteria::EPS, 20, 0.03));

        std::vector<cv::Point2f> backward_points;
        std::vector<unsigned char> backward_status;
        std::vector<float> backward_error;
        cv::calcOpticalFlowPyrLK(current_gray, previous_gray, current_points, backward_points,
                                backward_status, backward_error, cv::Size(21, 21), 3,
                                cv::TermCriteria(cv::TermCriteria::COUNT | cv::TermCriteria::EPS, 20, 0.03));

        std::vector<std::vector<float>> movements_x(box_count);
        std::vector<std::vector<float>> movements_y(box_count);
        std::vector<std::vector<float>> round_trip_errors(box_count);
        for (std::size_t point_index = 0; point_index < previous_points.size(); ++point_index) {
            if (!forward_status[point_index] || !backward_status[point_index]) {
                continue;
            }
            const cv::Point2f round_trip = backward_points[point_index] - previous_points[point_index];
            const float round_trip_error = std::sqrt(round_trip.dot(round_trip));
            if (!std::isfinite(round_trip_error) || round_trip_error > 1.5f ||
                forward_error[point_index] > 30.0f) {
                continue;
            }
            const std::size_t owner = owners[point_index];
            movements_x[owner].push_back(current_points[point_index].x - previous_points[point_index].x);
            movements_y[owner].push_back(current_points[point_index].y - previous_points[point_index].y);
            round_trip_errors[owner].push_back(round_trip_error);
        }

        for (std::size_t index = 0; index < box_count; ++index) {
            if (movements_x[index].size() < 4 || feature_counts[index] == 0) {
                continue;
            }
            const auto median = [](std::vector<float>& values) {
                const auto middle = values.begin() + static_cast<std::ptrdiff_t>(values.size() / 2);
                std::nth_element(values.begin(), middle, values.end());
                return *middle;
            };
            const float dx = median(movements_x[index]);
            const float dy = median(movements_y[index]);
            const float error = median(round_trip_errors[index]);
            const float x = boxes[index * 4] * width + dx;
            const float y = boxes[index * 4 + 1] * height + dy;
            const float w = boxes[index * 4 + 2] * width;
            const float h = boxes[index * 4 + 3] * height;
            if (x + w <= 0 || y + h <= 0 || x >= width || y >= height) {
                continue;
            }

            const float retained = static_cast<float>(movements_x[index].size()) /
                                   static_cast<float>(feature_counts[index]);
            const float confidence = std::clamp(retained * (1.0f - error / 1.5f), 0.0f, 1.0f);
            write_box(output[index], x, y, w, h, confidence, width, height);
        }

        // Lucas-Kanade is intentionally strict and normally rejects the
        // five-second gaps used by video sampling. ORB supplies a long-gap
        // fallback: match local face-region features into the new frame and
        // accept only a RANSAC-supported similarity transform. ArcFace still
        // verifies identity on the Rust side before a box can be persisted.
        const cv::Ptr<cv::ORB> orb = cv::ORB::create(1600);
        std::vector<cv::KeyPoint> current_keypoints;
        cv::Mat current_descriptors;
        orb->detectAndCompute(current_gray, cv::noArray(), current_keypoints, current_descriptors);
        if (!current_descriptors.empty()) {
            const cv::BFMatcher matcher(cv::NORM_HAMMING, false);
            for (std::size_t index = 0; index < box_count; ++index) {
                if (output[index].valid != 0) {
                    continue;
                }
                const float box_x = boxes[index * 4] * width;
                const float box_y = boxes[index * 4 + 1] * height;
                const float box_width = boxes[index * 4 + 2] * width;
                const float box_height = boxes[index * 4 + 3] * height;
                const float expand_x = box_width * 0.20f;
                const float expand_y = box_height * 0.20f;
                const int left = std::clamp(static_cast<int>(std::floor(box_x - expand_x)), 0, width - 1);
                const int top = std::clamp(static_cast<int>(std::floor(box_y - expand_y)), 0, height - 1);
                const int right = std::clamp(static_cast<int>(std::ceil(box_x + box_width + expand_x)), left + 1, width);
                const int bottom = std::clamp(static_cast<int>(std::ceil(box_y + box_height + expand_y)), top + 1, height);
                if (right - left < 24 || bottom - top < 24) {
                    continue;
                }

                std::vector<cv::KeyPoint> source_keypoints;
                cv::Mat source_descriptors;
                orb->detectAndCompute(previous_gray(cv::Rect(left, top, right - left, bottom - top)),
                                      cv::noArray(), source_keypoints, source_descriptors);
                if (source_descriptors.rows < 6) {
                    continue;
                }
                std::vector<std::vector<cv::DMatch>> pairs;
                matcher.knnMatch(source_descriptors, current_descriptors, pairs, 2);
                std::vector<cv::Point2f> source_matches;
                std::vector<cv::Point2f> current_matches;
                for (const auto& pair : pairs) {
                    if (pair.size() < 2 || pair[0].distance >= 0.82f * pair[1].distance || pair[0].distance > 80.0f) {
                        continue;
                    }
                    cv::Point2f source_point = source_keypoints[pair[0].queryIdx].pt;
                    source_point.x += static_cast<float>(left);
                    source_point.y += static_cast<float>(top);
                    source_matches.push_back(source_point);
                    current_matches.push_back(current_keypoints[pair[0].trainIdx].pt);
                }
                if (source_matches.size() < 4) {
                    continue;
                }

                cv::Mat inliers;
                const cv::Mat transform = cv::estimateAffinePartial2D(source_matches, current_matches, inliers,
                                                                       cv::RANSAC, 3.0, 2000, 0.99, 10);
                if (transform.empty()) {
                    continue;
                }
                const int inlier_count = cv::countNonZero(inliers);
                if (inlier_count < 4) {
                    continue;
                }
                const double a = transform.at<double>(0, 0);
                const double b = transform.at<double>(0, 1);
                const double scale = std::sqrt(a * a + b * b);
                if (!std::isfinite(scale) || scale < 0.60 || scale > 1.70) {
                    continue;
                }

                const std::vector<cv::Point2f> corners = {
                    {box_x, box_y},
                    {box_x + box_width, box_y},
                    {box_x, box_y + box_height},
                    {box_x + box_width, box_y + box_height},
                };
                std::vector<cv::Point2f> transformed;
                cv::transform(corners, transformed, transform);
                float min_x = transformed[0].x;
                float min_y = transformed[0].y;
                float max_x = transformed[0].x;
                float max_y = transformed[0].y;
                for (const cv::Point2f& point : transformed) {
                    min_x = std::min(min_x, point.x);
                    min_y = std::min(min_y, point.y);
                    max_x = std::max(max_x, point.x);
                    max_y = std::max(max_y, point.y);
                }
                if (max_x - min_x < 12.0f || max_y - min_y < 12.0f ||
                    max_x <= 0 || max_y <= 0 || min_x >= width || min_y >= height) {
                    continue;
                }
                const float inlier_ratio = static_cast<float>(inlier_count) /
                                           static_cast<float>(source_matches.size());
                const float support = std::min(1.0f, static_cast<float>(inlier_count) / 6.0f);
                write_box(output[index], min_x, min_y, max_x - min_x, max_y - min_y,
                          inlier_ratio * support, width, height);
            }
        }
        return 0;
    } catch (...) {
        return -2;
    }
}

} // extern "C"
