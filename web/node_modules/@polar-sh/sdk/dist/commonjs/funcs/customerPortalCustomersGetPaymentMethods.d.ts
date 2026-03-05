import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalCustomersGetPaymentMethodsRequest, CustomerPortalCustomersGetPaymentMethodsResponse, CustomerPortalCustomersGetPaymentMethodsSecurity } from "../models/operations/customerportalcustomersgetpaymentmethods.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * Get Customer Payment Methods
 *
 * @remarks
 * Get saved payment methods of the authenticated customer.
 *
 * **Scopes**: `customer_portal:read` `customer_portal:write`
 */
export declare function customerPortalCustomersGetPaymentMethods(client: PolarCore, security: CustomerPortalCustomersGetPaymentMethodsSecurity, request: CustomerPortalCustomersGetPaymentMethodsRequest, options?: RequestOptions): APIPromise<PageIterator<Result<CustomerPortalCustomersGetPaymentMethodsResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=customerPortalCustomersGetPaymentMethods.d.ts.map