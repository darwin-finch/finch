import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { BenefitsListRequest, BenefitsListResponse } from "../models/operations/benefitslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Benefits
 *
 * @remarks
 * List benefits.
 *
 * **Scopes**: `benefits:read` `benefits:write`
 */
export declare function benefitsList(client: PolarCore, request: BenefitsListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<BenefitsListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=benefitsList.d.ts.map