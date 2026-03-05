import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest } from "../models/operations/customerportaldownloadablescustomerportaldownloadablesget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Downloadable
 */
export declare function customerPortalDownloadablesGet(client: PolarCore, request: CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest, options?: RequestOptions): APIPromise<Result<any | undefined, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalDownloadablesGet.d.ts.map